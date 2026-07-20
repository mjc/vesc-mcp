{ defaultPackage }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.vesc-mcp;
  inherit (lib) types;
  package = if cfg.package != null then cfg.package else defaultPackage pkgs;
  httpBind = "${cfg.bind}:${toString cfg.port}";
  startupArgs = [ "--http" ]
    ++ lib.optional (!cfg.startup.refresh) "--skip-repository-refresh"
    ++ lib.optional (!cfg.startup.eagerIndex) "--skip-eager-index"
    ++ lib.optional (!cfg.startup.allowOfflineRestart) "--require-fresh-repositories";
  validRepositoryId = id: builtins.match "[a-z0-9][a-z0-9_-]*" id != null;
  validHttpsUrl = url:
    builtins.match "https://[^[:space:]]+" url != null
    && builtins.match ".*@.*" url == null;
  validRef = ref:
    builtins.match "refs/(heads|tags)/[A-Za-z0-9][A-Za-z0-9._/-]*" ref != null
    && builtins.match ".*[.][.].*" ref == null
    && builtins.match ".*//.*" ref == null;
  validSelector = selector:
    validRef selector || builtins.match "[0-9a-fA-F]{40}" selector != null;
  validGlob = glob:
    builtins.match "/.*" glob == null
    && builtins.match ".*[.][.].*" glob == null;
  repositoryType = types.submodule {
    options = {
      url = lib.mkOption { type = types.addCheck types.str validHttpsUrl; };
      defaultRef = lib.mkOption { type = types.addCheck types.str validRef; };
      enabled = lib.mkOption {
        type = types.bool;
        default = true;
      };
      required = lib.mkOption {
        type = types.bool;
        default = true;
      };
      include = lib.mkOption {
        type = types.listOf (types.addCheck types.str validGlob);
        default = [ ];
      };
      exclude = lib.mkOption {
        type = types.listOf (types.addCheck types.str validGlob);
        default = [ ];
      };
      trustTier = lib.mkOption {
        type = types.enum [
          "official"
          "community"
          "untrusted"
        ];
        default = "official";
      };
      license = lib.mkOption { type = types.nonEmptyStr; };
      attribution = lib.mkOption { type = types.nonEmptyStr; };
      maxFileBytes = lib.mkOption {
        type = types.ints.positive;
        default = 1024 * 1024;
      };
      maxFiles = lib.mkOption {
        type = types.ints.positive;
        default = 100000;
      };
      maxTotalBytes = lib.mkOption {
        type = types.ints.positive;
        default = 1024 * 1024 * 1024;
      };
    };
  };
  repositoryIds = builtins.attrNames cfg.repositories;
  selectionType = types.attrsOf (types.addCheck types.str validSelector);
  selectedRef = id: repository:
    cfg.defaultVersions.${id} or repository.defaultRef;
  repositoryConfig = id:
    let
      repository = cfg.repositories.${id};
    in
    {
      inherit id;
      remote_url = repository.url;
      default_ref = selectedRef id repository;
      policy = if !repository.enabled then "disabled" else if repository.required then "required" else "optional";
      include = repository.include;
      exclude = repository.exclude;
      trust_tier = repository.trustTier;
      inherit (repository) license attribution;
      max_file_bytes = repository.maxFileBytes;
      max_files = repository.maxFiles;
      max_total_bytes = repository.maxTotalBytes;
    };
  toml = pkgs.formats.toml { };
  semanticConfig = {
    idle_timeout_secs = cfg.semanticIdleTimeoutSecs;
  }
  // lib.optionalAttrs (cfg.semanticModelDir != null) {
    model_dir = toString cfg.semanticModelDir;
  }
  // lib.optionalAttrs (cfg.semanticModelId != null) {
    model_id = cfg.semanticModelId;
  }
  // lib.optionalAttrs (cfg.semanticModelRevision != null) {
    model_revision = cfg.semanticModelRevision;
  };
  runtimeConfig = toml.generate "vesc-mcp.toml" {
    paths.package_roots = map toString cfg.packageRoots;
    knowledge = {
      mode = cfg.retrievalMode;
      data_root = "/var/lib/${cfg.stateDirectory}";
      repositories = map repositoryConfig repositoryIds;
      prewarm = cfg.prewarm;
      semantic = semanticConfig;
    }
    // lib.optionalAttrs (cfg.artifactPath != null) {
      artifact_path = toString cfg.artifactPath;
    }
    ;
  };
  environment = {
    VESC_MCP_HTTP_BIND = httpBind;
    VESC_MCP_HTTP_PATH = cfg.path;
    VESC_MCP_HTTP_ALLOWED_HOSTS = lib.concatStringsSep "," cfg.allowedHosts;
    VESC_MCP_HTTP_ALLOWED_ORIGINS = lib.concatStringsSep "," cfg.allowedOrigins;
    VESC_MCP_CONFIG = runtimeConfig;
  };
in
{
  options.services.vesc-mcp = {
    enable = lib.mkEnableOption "the shared VESC MCP Streamable HTTP service";
    package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "vesc-mcp package to run.";
    };
    bind = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      description = "Listen address. Keep the local default unless remote exposure is intentional.";
    };
    port = lib.mkOption {
      type = lib.types.port;
      default = 8080;
    };
    path = lib.mkOption {
      type = lib.types.strMatching "/.*";
      default = "/mcp";
    };
    allowedHosts = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [
        "localhost"
        "127.0.0.1"
        "::1"
      ];
      description = "Host authorities accepted by rmcp's DNS-rebinding protection.";
    };
    allowedOrigins = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
    };
    authTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "EnvironmentFile containing VESC_MCP_HTTP_AUTH_TOKEN for bearer auth.";
    };
    packageRoots = lib.mkOption {
      type = lib.types.listOf lib.types.path;
      default = [ ];
      description = "Roots reserved for future authenticated package-tool policy; HTTP exposes knowledge tools only.";
    };
    artifactPath = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
    };
    retrievalMode = lib.mkOption {
      type = lib.types.enum [
        "lexical"
        "legacy"
        "auto"
        "hybrid"
      ];
      default = "auto";
    };
    semanticModelDir = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
    };
    semanticModelId = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
    };
    semanticModelRevision = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
    };
    semanticIdleTimeoutSecs = lib.mkOption {
      type = types.ints.unsigned;
      default = 300;
    };
    stateDirectory = lib.mkOption {
      type = types.strMatching "[A-Za-z0-9_.-]+";
      default = "vesc-mcp";
      description = "systemd StateDirectory containing bare repositories, manifests, and indexes.";
    };
    cacheDirectory = lib.mkOption {
      type = types.strMatching "[A-Za-z0-9_.-]+";
      default = "vesc-mcp";
      description = "systemd CacheDirectory for independently disposable caches and models.";
    };
    repositories = lib.mkOption {
      type = types.addCheck (types.attrsOf repositoryType) (repositories:
        builtins.all validRepositoryId (builtins.attrNames repositories));
      default = { };
      description = "Approved managed Git repositories keyed by stable repository ID.";
    };
    defaultVersions = lib.mkOption {
      type = selectionType;
      default = { };
      description = "Default ref overrides keyed by configured repository ID.";
    };
    prewarm = lib.mkOption {
      type = types.listOf selectionType;
      default = [ ];
      description = "Historical repository-version sets to prepare eagerly.";
    };
    startup = {
      refresh = lib.mkOption {
        type = types.bool;
        default = true;
        description = "Refresh managed bare repositories before serving.";
      };
      eagerIndex = lib.mkOption {
        type = types.bool;
        default = true;
        description = "Prepare the default and prewarmed snapshots before serving.";
      };
      allowOfflineRestart = lib.mkOption {
        type = types.bool;
        default = true;
        description = "Serve the last valid cached snapshot when refresh fails.";
      };
      timeoutSecs = lib.mkOption {
        type = types.ints.positive;
        default = 900;
        description = "Maximum systemd startup duration, including refresh and eager indexing.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = builtins.all (id: builtins.elem id repositoryIds) (builtins.attrNames cfg.defaultVersions);
        message = "services.vesc-mcp.defaultVersions may only name configured repositories";
      }
      {
        assertion = builtins.all
          (selection: builtins.all (id: builtins.elem id repositoryIds) (builtins.attrNames selection))
          cfg.prewarm;
        message = "services.vesc-mcp.prewarm may only name configured repositories";
      }
    ];
    systemd.services.vesc-mcp = {
      description = "Shared VESC MCP Streamable HTTP service";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      inherit environment;
      serviceConfig = {
        ExecStart = "${package}/bin/vesc-mcp-server ${lib.concatStringsSep " " startupArgs}";
        Restart = "on-failure";
        RestartSec = 2;
        DynamicUser = true;
        StateDirectory = cfg.stateDirectory;
        CacheDirectory = cfg.cacheDirectory;
        TimeoutStartSec = cfg.startup.timeoutSecs;
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = "read-only";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        EnvironmentFile = lib.optional (cfg.authTokenFile != null) cfg.authTokenFile;
      };
    };
  };
}
