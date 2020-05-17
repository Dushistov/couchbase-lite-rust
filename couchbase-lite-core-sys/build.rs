use bindgen::{Builder, RustTarget};
use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
};

fn main() {
    let dst = cmake::Config::new(Path::new("couchbase-lite-core"))
        .define("DISABLE_LTO_BUILD", "True")
        .build_target("LiteCore")
        .build()
        .join("build");

    let out_dir = getenv_unwrap("OUT_DIR");
    let out_dir = Path::new(&out_dir);

    let lib_name = if cfg!(target_os = "windows") {
        "LiteCore.dll"
    } else if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
        "libLiteCore.dylib"
    } else {
        "libLiteCore.so"
    };
    // so it is possible to use from project that uses Rust library,
    // but not on Rust language
    let target_dir = target_directory(out_dir);

    if cfg!(target_os = "windows") {
        let msvc_cmake_profile = match &getenv_unwrap("PROFILE")[..] {
            "debug" => "Debug",
            "release" | "bench" => "Release",
            unknown => {
                eprintln!(
                    "Warning: unknown Rust profile={}; defaulting to a release build.",
                    unknown
                );
                "Release"
            }
        };
        let dst = dst.join(msvc_cmake_profile);
        if let Err(err) = fs::copy(dst.join(lib_name), target_dir.join(lib_name)) {
            panic!(
                "copy {} from '{}' to '{}' faied: {}",
                lib_name,
                dst.display(),
                target_dir.display(),
                err
            );
        }
        let lib_lib_name = "LiteCore.lib";
        if let Err(err) = fs::copy(dst.join(lib_lib_name), target_dir.join(lib_lib_name)) {
            panic!(
                "copy {} from '{}' to '{}' faied: {}",
                lib_lib_name,
                dst.display(),
                target_dir.display(),
                err
            );
        }
    } else {
        if let Err(err) = fs::copy(dst.join(lib_name), target_dir.join(lib_name)) {
            panic!(
                "copy {} from '{}' to '{}' faied: {}",
                lib_name,
                dst.display(),
                target_dir.display(),
                err
            );
        }
    }
    println!("cargo:rustc-link-search=native={}", target_dir.display());
    println!("cargo:rustc-link-lib=dylib=LiteCore");

    let mut includes = vec![
        Path::new("couchbase-lite-core").join("C").join("include"),
        Path::new("couchbase-lite-core")
            .join("vendor")
            .join("fleece")
            .join("API"),
        Path::new("couchbase-lite-core").into(),
        Path::new(".").into(),
    ];
    let target = getenv_unwrap("TARGET");
    let mut framework_dirs = vec![];

    if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
        let (mut addon_include_dirs, mut addon_framework_dirs) =
            cc_system_include_dirs().expect("get system include directories from cc failed");
        includes.append(&mut addon_include_dirs);
        framework_dirs.append(&mut addon_framework_dirs);
    }
    run_bindgen_for_c_headers(
        &target,
        &includes,
        &framework_dirs,
        &[
            "c4.h",
            "fleece/FLSlice.h",
            "c4Document+Fleece.h",
            "fleece/Fleece.h",
            "Replicator/CivetWebSocket.hh",
            "couch_lite_log_retrans.hpp",
        ],
        &out_dir.join("c4_header.rs"),
    )
    .expect("bindgen failed");

    let mut cc_builder = cc::Build::new();

    for inc in &includes {
        cc_builder.include(inc);
    }

    cc_builder
        .cpp(true)
        .flag_if_supported("-std=c++11")
        .file("couch_lite_log_retrans.cpp")
        .compile("couch_lite_log_retrans");
}

/// Convert something like
/// project/target/release/build/lib-couchbase-lite-core-sys-9daa760ee41fe7b8/out
/// to
/// project/target/release
/// Waiting https://github.com/rust-lang/cargo/issues/5457 for proper mechanizm
fn target_directory(out_dir: &Path) -> &Path {
    out_dir
        .ancestors()
        .nth(3)
        .expect("Can not find ancestor during cargo_target_dir search")
}

fn run_bindgen_for_c_headers<P: AsRef<Path>>(
    target: &str,
    include_dirs: &[P],
    framework_dirs: &[P],
    c_headers: &[&str],
    output_rust: &Path,
) -> Result<(), String> {
    assert!(!c_headers.is_empty());
    let c_file_path = search_file_in_directory(include_dirs, c_headers[0])
        .map_err(|_| format!("Can not find {}", c_headers[0]))?;

    let mut dependicies = Vec::with_capacity(c_headers.len());
    for header in c_headers.iter() {
        let c_file_path = search_file_in_directory(include_dirs, header)
            .map_err(|_| format!("Can not find {}", header))?;
        dependicies.push(c_file_path);
    }

    if let Ok(out_meta) = output_rust.metadata() {
        let mut res_recent_enough = true;
        for c_file_path in &dependicies {
            let c_meta = c_file_path.metadata().map_err(|err| err.to_string())?;
            if c_meta.modified().unwrap() >= out_meta.modified().unwrap() {
                res_recent_enough = false;
                break;
            }
        }
        if res_recent_enough {
            return Ok(());
        }
    }

    let mut bindings: Builder = bindgen::builder()
        .header(c_file_path.to_str().unwrap())
        .generate_comments(false)
        .prepend_enum_name(true)
        .size_t_is_usize(true)
        .rustfmt_bindings(false);
    bindings = include_dirs.iter().fold(bindings, |acc, x| {
        acc.clang_arg("-I".to_string() + x.as_ref().to_str().unwrap())
    });

    bindings = framework_dirs.iter().fold(bindings, |acc, x| {
        acc.clang_arg("-F".to_string() + x.as_ref().to_str().unwrap())
    });

    bindings = bindings
        .rust_target(RustTarget::Stable_1_21)
        .opaque_type("timex")//to big reserved part for Debug
        .blacklist_type("max_align_t")//long double not supported yet,
                                      // see https://github.com/servo/rust-bindgen/issues/550
        ;
    bindings = if target.contains("windows") {
        //see https://github.com/servo/rust-bindgen/issues/578
        bindings.trust_clang_mangling(false)
    } else {
        bindings
    };

    bindings =
        c_headers[1..]
            .iter()
            .fold(Ok(bindings), |acc: Result<Builder, String>, header| {
                let c_file_path = search_file_in_directory(include_dirs, header)
                    .map_err(|_| format!("Can not find {}", header))?;
                let c_file_str = c_file_path
                    .to_str()
                    .ok_or_else(|| format!("Invalid unicode in path to {}", header))?;
                Ok(acc.unwrap().clang_arg("-include").clang_arg(c_file_str))
            })?;
    let bindings = bindings.rustified_enum("FLValueType");
    let generated_bindings = bindings
        .generate()
        .map_err(|_| "Failed to generate bindings".to_string())?;
    generated_bindings
        .write_to_file(output_rust)
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn search_file_in_directory<P>(dirs: &[P], file: &str) -> Result<PathBuf, ()>
where
    P: AsRef<Path>,
{
    for dir in dirs {
        let file_path = dir.as_ref().join(file);
        if file_path.exists() && file_path.is_file() {
            return Ok(file_path);
        }
    }
    Err(())
}

fn cc_system_include_dirs() -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let cc_build = cc::Build::new();

    let cc_process = cc_build
        .get_compiler()
        .to_command()
        .args(&["-v", "-x", "c", "-E", "-"])
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .spawn()
        .map_err(|err| err.to_string())?;

    cc_process
        .stdin
        .ok_or_else(|| format!("can not get stdin of cc"))?
        .write_all(b"\n")
        .map_err(|err| err.to_string())?;

    let mut cc_output = String::new();

    cc_process
        .stderr
        .ok_or_else(|| format!("can not get stderr of cc"))?
        .read_to_string(&mut cc_output)
        .map_err(|err| err.to_string())?;

    const BEGIN_PAT: &str = "\n#include <...> search starts here:\n";
    const END_PAT: &str = "\nEnd of search list.\n";
    let start_includes = cc_output
        .find(BEGIN_PAT)
        .ok_or_else(|| format!("No '{}' in output from cc", BEGIN_PAT))?
        + BEGIN_PAT.len();
    let end_includes = (&cc_output[start_includes..])
        .find(END_PAT)
        .ok_or_else(|| format!("No '{}' in output from cc", END_PAT))?
        + start_includes;

    const FRAMEWORK_PAT: &str = " (framework directory)";

    let include_dis = (&cc_output[start_includes..end_includes])
        .split('\n')
        .filter_map(|s| {
            if !s.ends_with(FRAMEWORK_PAT) {
                Some(PathBuf::from(s.trim().to_string()))
            } else {
                None
            }
        })
        .collect();
    let framework_dirs = (&cc_output[start_includes..end_includes])
        .split('\n')
        .filter_map(|s| {
            if s.ends_with(FRAMEWORK_PAT) {
                let line = s.trim();
                let line = &line[..line.len() - FRAMEWORK_PAT.len()];
                Some(PathBuf::from(line.trim().to_string()))
            } else {
                None
            }
        })
        .collect();
    Ok((include_dis, framework_dirs))
}

fn getenv_unwrap(v: &str) -> String {
    match env::var(v) {
        Ok(s) => s,
        Err(..) => fail(&format!("environment variable `{}` not defined", v)),
    }
}

fn fail(s: &str) -> ! {
    panic!("\n{}\n\nbuild script failed, must exit now", s)
}
