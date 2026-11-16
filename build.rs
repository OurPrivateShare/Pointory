use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    slint_build::compile("ui/app.slint").expect("Failed to compile Slint UI");
    println!("cargo:rerun-if-changed=assets/pointory.ico");
    println!("cargo:rerun-if-env-changed=WindowsSdkVerBinPath");
    println!("cargo:rerun-if-env-changed=WindowsSdkDir");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources();
    }
}

fn embed_windows_resources() {
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        Ok("msvc"),
        "Pointory Windows releases require the MSVC toolchain"
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let source = output.join("pointory.rc");
    let resource = output.join("pointory.res");
    let icon = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("assets/pointory.ico")
        .to_string_lossy()
        .replace('\\', "\\\\");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut numbers = version.split('.').map(|part| part.parse::<u16>().unwrap());
    let numeric_version = format!(
        "{},{},{},0",
        numbers.next().unwrap(),
        numbers.next().unwrap(),
        numbers.next().unwrap()
    );
    fs::write(
        &source,
        format!(
            r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric_version}
PRODUCTVERSION {numeric_version}
FILEFLAGSMASK 0x3fL
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Pointory\0"
            VALUE "FileDescription", "Pointory - Desktop Mouse Clicker\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "InternalName", "pointory\0"
            VALUE "OriginalFilename", "Pointory.exe\0"
            VALUE "ProductName", "Pointory\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
        ),
    )
    .expect("Failed to write Windows application resources");
    let compiler = find_resource_compiler();
    let result = Command::new(&compiler)
        .arg("/nologo")
        .arg("/c65001")
        .arg("/fo")
        .arg(&resource)
        .arg(&source)
        .output()
        .expect("Failed to start Windows resource compiler");
    assert!(
        result.status.success(),
        "Windows resource compiler failed ({}): {}{}",
        compiler.display(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    println!("cargo:rustc-link-arg-bin=pointory={}", resource.display());
}

fn find_resource_compiler() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(directory) = env::var_os("WindowsSdkVerBinPath") {
        let directory = PathBuf::from(directory);
        candidates.push(directory.join("x64/rc.exe"));
        candidates.push(directory.join("rc.exe"));
    }
    if let Some(path) = env::var_os("PATH") {
        candidates.extend(env::split_paths(&path).map(|directory| directory.join("rc.exe")));
    }
    let sdk_bin = env::var_os("WindowsSdkDir")
        .map(PathBuf::from)
        .map(|directory| directory.join("bin"))
        .or_else(|| {
            env::var_os("ProgramFiles(x86)")
                .map(PathBuf::from)
                .map(|directory| directory.join("Windows Kits/10/bin"))
        });
    if let Some(sdk_bin) = sdk_bin {
        let mut versions = fs::read_dir(&sdk_bin)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("10."))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        versions.sort_by_cached_key(|directory| {
            directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .split('.')
                .filter_map(|part| part.parse::<u32>().ok())
                .collect::<Vec<_>>()
        });
        candidates.extend(
            versions
                .into_iter()
                .rev()
                .map(|directory| directory.join("x64/rc.exe")),
        );
        candidates.push(sdk_bin.join("x64/rc.exe"));
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .expect(
            "Windows SDK rc.exe was not found; install the Windows SDK with the MSVC build tools",
        )
}
