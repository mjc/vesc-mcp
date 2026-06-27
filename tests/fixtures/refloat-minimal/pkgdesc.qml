import QtQuick 2.15

Item {
    property string pkgName: "Refloat Minimal"
    property string pkgDescriptionMd: "package_README-gen.md"
    property string pkgLisp: "lisp/package.lisp"
    property string pkgQml: "ui.qml"
    property bool pkgQmlIsFullscreen: false
    property string pkgOutput: "refloat-minimal.vescpkg"

    function isCompatible (fwRxParams) {
        return true;
    }
}
