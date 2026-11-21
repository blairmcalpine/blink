//! Embeds the icon, version information, and application manifest in
//! `blink.exe` when building for Windows. Other targets get nothing.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// The copyright holder named in `LICENSE-MIT`.
const COPYRIGHT: &str = "Copyright (c) 2026 The blink contributors";

fn main() {
    if let Err(error) = run() {
        println!("cargo::error={error}");
    }
}

fn run() -> Result<(), String> {
    let assets = PathBuf::from(var("CARGO_MANIFEST_DIR")?).join("assets");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed={}", assets.display());

    if var("CARGO_CFG_TARGET_OS")? != "windows" {
        return Ok(());
    }

    let rc = resource_script(&assets)?;
    let rc_path = PathBuf::from(var("OUT_DIR")?).join("blink.rc");
    fs::write(&rc_path, rc).map_err(|error| format!("writing {}: {error}", rc_path.display()))?;
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .map_err(|error| format!("compiling {}: {error}", rc_path.display()))
}

/// The resource script: the icon, the manifest (resource type 24, ID 1, which
/// Windows reads at process creation), and a VERSIONINFO block. It spells out
/// the `winver.h` constants so no Windows SDK headers are needed.
fn resource_script(assets: &Path) -> Result<String, String> {
    let version = var("CARGO_PKG_VERSION")?;
    let numeric = ["MAJOR", "MINOR", "PATCH"]
        .iter()
        .map(|part| {
            let name = format!("CARGO_PKG_VERSION_{part}");
            let value = var(&name)?;
            value
                .parse::<u16>()
                .map_err(|_| format!("{name}={value} does not fit a Windows version field"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let numeric = numeric
        .iter()
        .map(u16::to_string)
        .chain(["0".to_owned()])
        .collect::<Vec<_>>()
        .join(",");
    let icon = rc_path(&assets.join("blink.ico"));
    let manifest = rc_path(&assets.join("blink.exe.manifest"));

    Ok(format!(
        r#"1 ICON "{icon}"
1 24 "{manifest}"

1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "The blink contributors"
            VALUE "FileDescription", "blink"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "blink"
            VALUE "LegalCopyright", "{COPYRIGHT}"
            VALUE "OriginalFilename", "blink.exe"
            VALUE "ProductName", "blink"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    ))
}

/// `path` as a resource script string literal body.
fn rc_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\"\"")
}

fn var(name: &str) -> Result<String, String> {
    env::var(name).map_err(|error| format!("{name}: {error}"))
}
