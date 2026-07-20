{ defaultPackage }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.vesc-mcp;
  package = if cfg.package != null then cfg.package else defaultPackage pkgs;
  httpBind = "${cfg.bind}:${toString cfg.port}";
  environment = {
    VESC_MCP_HTTP_BIND = httpBind;
    VESC_MCP_HTTP_PATH = cfg.path;
    VESC_MCP_HTTP_ALLOWED_HOSTS = lib.concatStringsSep "," cfg.allowedHosts;
    VESC_MCP_HTTP_ALLOWED_ORIGINS = lib.concatStringsSep "," cfg.allowedOrigins;
    VESC_RAG_MODE = cfg.retrievalMode;
    VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS = toString cfg.semanticIdleTimeoutSecs;
    VESC_PACKAGE_ROOTS = lib.concatStringsSep ":" (map toString cfg.packageRoots);
  }
  // lib.optionalAttrs (cfg.artifactPath != null) {
    VESC_RAG_ARTIFACT = toString cfg.artifactPath;
  }
  // lib.optionalAttrs (cfg.semanticModelDir != null) {
    VESC_RAG_SEMANTIC_MODEL_DIR = toString cfg.semanticModelDir;
  }
  // lib.optionalAttrs (cfg.semanticModelId != null) {
    VESC_RAG_SEMANTIC_MODEL_ID = cfg.semanticModelId;
  }
  // lib.optionalAttrs (cfg.semanticModelRevision != null) {
    VESC_RAG_SEMANTIC_MODEL_REVISION = cfg.semanticModelRevision;
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
      type = lib.types.strMatching "^/";
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
      type = lib.types.nonnegativeInt;
      default = 300;
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.vesc-mcp = {
      description = "Shared VESC MCP Streamable HTTP service";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      inherit environment;
      serviceConfig = {
        ExecStart = "${package}/bin/vesc-mcp-server --http";
        Restart = "on-failure";
        RestartSec = 2;
        DynamicUser = true;
        StateDirectory = "vesc-mcp";
        CacheDirectory = "vesc-mcp";
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
