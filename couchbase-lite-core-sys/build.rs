use bindgen::{Builder, CargoCallbacks, RustTarget};
use std::{
    env,
    path::{Path, PathBuf},
};

#[cfg(feature = "docs-rs")]
fn main() {}

#[cfg(not(feature = "docs-rs"))]
fn main() {
    env_logger::init();
    let target = getenv_unwrap("TARGET");
    let is_msvc = target.contains("msvc");

    if cfg!(feature = "with-asan") && !cfg!(feature = "build") {
        panic!("Invalid set of options: with-asan should be used with build");
    }

    let sdir = download_source_code_via_git_if_needed().expect("download of source code failed");

    let bdirs = cmake_build_src_dir(&sdir, is_msvc);
    println!("build directory: {:?}\nsource directory {:?}", bdirs, sdir);

    if bdirs.is_empty() {
        panic!("You didn't specify build directory for couchbase-lite-core");
    } else if bdirs.len() == 1 {
        specify_library_search_dirs_for_std_layout(&bdirs[0]);
    } else {
        for d in &bdirs {
            println!("cargo:rustc-link-search=native={}", d.display());
        }
    }

    if cfg!(feature = "use-couchbase-lite-sqlite") {
        println!("cargo:rustc-link-lib=static=CouchbaseSqlite3");
    }
    if cfg!(feature = "use-couchbase-lite-websocket") {
        println!("cargo:rustc-link-lib=static=LiteCoreWebSocket");
    }
    println!("cargo:rustc-link-lib=static=LiteCoreStatic");
    println!("cargo:rustc-link-lib=static=FleeceStatic");
    println!("cargo:rustc-link-lib=static=SQLite3_UnicodeSN");
    println!("cargo:rustc-link-lib=static=BLIPStatic");
    println!("cargo:rustc-link-lib=static=mbedcrypto");
    println!("cargo:rustc-link-lib=static=mbedtls");
    println!("cargo:rustc-link-lib=static=mbedx509");

    if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=icuuc");
        println!("cargo:rustc-link-lib=icui18n");
        println!("cargo:rustc-link-lib=icudata");
        println!("cargo:rustc-link-lib=z");
        println!("cargo:rustc-link-lib=stdc++");
    } else if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=z");
        //TODO: remove this dependicies: CoreFoundation + Foundation
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=SystemConfiguration");
        println!("cargo:rustc-link-lib=framework=Security");
        println!("cargo:rustc-link-lib=c++");
    } else if is_msvc {
        println!("cargo:rustc-link-lib=ws2_32");
    }

    let mut includes = vec![
        sdir.join("C").join("include"),
        sdir.join("vendor").join("fleece").join("API"),
        sdir.clone(),
    ];

    let (mut addon_include_dirs, framework_dirs) =
        cc_system_include_dirs().expect("get system include directories from cc failed");
    includes.append(&mut addon_include_dirs);

    let out_dir = getenv_unwrap("OUT_DIR");
    let out_dir = Path::new(&out_dir);

    let mut headers = vec![
        "c4.h",
        "fleece/FLSlice.h",
        "c4Document+Fleece.h",
        "fleece/Fleece.h",
        "fleece/FLExpert.h",
    ];
    if cfg!(feature = "use-couchbase-lite-websocket") {
        headers.push("c4Private.h");
        includes.push(sdir.join("C"));
    }

    run_bindgen_for_c_headers(
        &target,
        &includes,
        &framework_dirs,
        &headers,
        &out_dir.join("c4_header.rs"),
    )
    .expect("bindgen failed");
}

fn specify_library_search_dirs_for_std_layout(bdir: &Path) {
    println!("cargo:rustc-link-search=native={}", bdir.display());
    println!(
        "cargo:rustc-link-search=native={}",
        bdir.join("vendor").join("fleece").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        bdir.join("Networking").join("BLIP").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        bdir.join("vendor").join("sqlite3-unicodesn").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        bdir.join("vendor")
            .join("mbedtls")
            .join("crypto")
            .join("library")
            .display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        bdir.join("vendor")
            .join("mbedtls")
            .join("library")
            .display()
    );
}

#[cfg(feature = "git-download")]
fn download_source_code_via_git_if_needed() -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::{fs, process::Command, str};
    use which::which;

    const URL: &str = "https://github.com/couchbase/couchbase-lite-core";
    const COMMIT_SHA1: &str = "bdb7abe064a3366281b2399bf823cfe6a255f7d2";

    let git_path = which("git")?;
    let cur_dir = env::current_dir()?;
    let parent_dir = cur_dir
        .parent()
        .ok_or_else(|| format!("Can not find parent directory for current"))?;
    let src_dir = Path::new(&parent_dir).join("couchbase-lite-core");

    if !src_dir.exists() {
        fs::create_dir(&src_dir)?;
    }

    let run_git_cmd = |args: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
        let mut child = Command::new(&git_path)
            .args(args)
            .current_dir(&src_dir)
            .spawn()?;
        let ecode = child.wait()?;

        if ecode.success() {
            Ok(())
        } else {
            Err(format!("git {args:?} failed").into())
        }
    };

    if !src_dir.join(".git").exists() {
        run_git_cmd(&["init"])?;
    }
    let output = Command::new(&git_path)
        .arg("remote")
        .arg("-v")
        .current_dir(&src_dir)
        .output()?;
    if !output.status.success() {
        return Err("git remote -v failed".into());
    }
    let remote_output = str::from_utf8(&output.stdout)?;
    let remote_output = remote_output.trim();
    println!("git remote -v output: {remote_output}");
    if remote_output.is_empty() {
        run_git_cmd(&["remote", "add", "origin", URL])?;
    }

    let mut commit_ok = false;
    let output = Command::new(&git_path)
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(&src_dir)
        .output()?;
    if output.status.success() {
        let current_commit = str::from_utf8(&output.stdout)?;
        let current_commit = current_commit.trim();
        if current_commit == COMMIT_SHA1 {
            println!("git repo has proper commit: {current_commit}");
            commit_ok = true;
        }
    }
    if !commit_ok {
        println!("fetching {COMMIT_SHA1} from {URL}");
        run_git_cmd(&["fetch", "--depth", "1", "origin", COMMIT_SHA1])?;
        run_git_cmd(&["reset", "--hard", "FETCH_HEAD"])?;
        run_git_cmd(&[
            "submodule",
            "update",
            "--depth",
            "1",
            "--init",
            "--recursive",
        ])?;
    }

    Ok(src_dir.into())
}

#[cfg(not(feature = "git-download"))]
fn download_source_code_via_git_if_needed() -> Result<PathBuf, Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=COUCHBASE_LITE_CORE_SRC_DIR");
    Ok(getenv_unwrap("COUCHBASE_LITE_CORE_SRC_DIR").into())
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
    /*
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
    }*/
    let mut bindings: Builder = bindgen::builder()
        .header(c_file_path.to_str().unwrap())
        .generate_comments(false)
        .prepend_enum_name(true)
        .size_t_is_usize(true)
        .allowlist_type("C4.*")
        .allowlist_var("k.*")
        .allowlist_function("c4.*")
        .allowlist_function("k?C4.*")
        .allowlist_type("FL.*")
        .allowlist_function("_?FL.*")
        .newtype_enum("FLError")
        .newtype_enum("C4.*")
        .rustified_enum("FLValueType")
        .rustified_enum("FLTrust")
        .rustified_enum("C4QueryLanguage")
        .no_copy("FLSliceResult")
        // we not use string_view, and there is bindgen's bug:
        // https://github.com/rust-lang/rust-bindgen/issues/2152
        .layout_tests(false)
        .opaque_type("std::string_view")
        .opaque_type("std::string")
        .rustfmt_bindings(true)
        // clang args to deal with C4_ENUM/C4_OPTIONS
        .clang_arg("-x")
        .clang_arg("c++")
        .clang_arg("-std=c++17")
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed
        .parse_callbacks(Box::new(CargoCallbacks));

    bindings = include_dirs.iter().fold(bindings, |acc, x| {
        acc.clang_arg("-I".to_string() + x.as_ref().to_str().unwrap())
    });
    bindings = framework_dirs.iter().fold(bindings, |acc, x| {
        acc.clang_arg("-F".to_string() + x.as_ref().to_str().unwrap())
    });

    bindings = bindings
        .rust_target(RustTarget::Stable_1_47)
        .opaque_type("timex")//to big reserved part for Debug
        .blocklist_type("max_align_t")//long double not supported yet,
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
    let generated_bindings = bindings
        .generate()
        .map_err(|_| "Failed to generate bindings".to_string())?;
    generated_bindings
        .write_to_file(output_rust)
        .map_err(|err| err.to_string())?;
    Ok(())
}

#[cfg(feature = "build")]
fn cmake_build_src_dir(src_dir: &Path, is_msvc: bool) -> Vec<PathBuf> {
    let mut cmake_config = cmake::Config::new(src_dir);
    cmake_config
        .define("DISABLE_LTO_BUILD", "True")
        .define("ENABLE_TESTING", "False")
        .define("LITECORE_BUILD_TESTS", "False");
    if !cfg!(feature = "use-couchbase-lite-sqlite") {
        println!("disable build of sqlite");
        cmake_config.define("MAINTAINER_MODE", "False");
    }
    if cfg!(feature = "with-asan") {
        let cc_flags = "-fno-omit-frame-pointer -fsanitize=address";
        let ld_flags = "-fsanitize=address";
        cmake_config
            .define("CMAKE_C_FLAGS", cc_flags)
            .define("CMAKE_CXX_FLAGS", cc_flags)
            .define("CMAKE_MODULE_LINKER_FLAGS", ld_flags)
            .define("CMAKE_SHARED_LINKER_FLAGS", ld_flags);
    }

    cmake_config.build_target(if !is_msvc { "all" } else { "ALL_BUILD" });
    let cmake_profile = cmake_config.get_profile().to_string();
    let dst = cmake_config.build().join("build");

    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=CXX");

    vec![if !is_msvc {
        dst
    } else {
        dst.join(cmake_profile)
    }]
}

#[cfg(not(feature = "build"))]
fn cmake_build_src_dir(_src_dir: &Path, _is_msvc: bool) -> Vec<PathBuf> {
    if let Ok(dirs) = env::var("COUCHBASE_LITE_CORE_BUILD_DIRS") {
        println!("cargo:rerun-if-env-changed=COUCHBASE_LITE_CORE_BUILD_DIRS");
        let mut ret = vec![];
        for d in dirs.split('^') {
            let d: PathBuf = d.into();
            if !ret.iter().any(|e| *e == d) {
                ret.push(d);
            }
        }
        return ret;
    }
    println!("cargo:rerun-if-env-changed=COUCHBASE_LITE_CORE_BUILD_DIR");
    vec![getenv_unwrap("COUCHBASE_LITE_CORE_BUILD_DIR").into()]
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn cc_system_include_dirs() -> Result<(Vec<PathBuf>, Vec<PathBuf>), Box<dyn std::error::Error>> {
    use std::{
        io::{Read, Write},
        process::Stdio,
    };

    let mut include_dirs = Vec::new();
    let mut framework_dirs = Vec::new();

    fn extend_unique(v: &mut Vec<PathBuf>, addon: impl IntoIterator<Item = PathBuf>) {
        for it in addon {
            if !v.iter().any(|e| *e == it) {
                v.push(it);
            }
        }
    }

    for lang in &["c", "c++"] {
        let cc_build = cc::Build::new();

        let cc_process = cc_build
            .get_compiler()
            .to_command()
            .env("LANG", "C")
            .env("LC_MESSAGES", "C")
            .args(&["-v", "-x", lang, "-E", "-"])
            .stderr(Stdio::piped())
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .spawn()?;

        cc_process
            .stdin
            .ok_or_else(|| "can not get stdin of cc".to_string())?
            .write_all(b"\n")?;

        let mut cc_output = String::new();

        cc_process
            .stderr
            .ok_or_else(|| "can not get stderr of cc".to_string())?
            .read_to_string(&mut cc_output)?;

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

        extend_unique(
            &mut include_dirs,
            (&cc_output[start_includes..end_includes])
                .split('\n')
                .filter_map(|s| {
                    if !s.ends_with(FRAMEWORK_PAT) {
                        Some(PathBuf::from(s.trim().to_string()))
                    } else {
                        None
                    }
                }),
        );

        extend_unique(
            &mut framework_dirs,
            (&cc_output[start_includes..end_includes])
                .split('\n')
                .filter_map(|s| {
                    if s.ends_with(FRAMEWORK_PAT) {
                        let line = s.trim();
                        let line = &line[..line.len() - FRAMEWORK_PAT.len()];
                        Some(PathBuf::from(line.trim().to_string()))
                    } else {
                        None
                    }
                }),
        );
    }

    if cfg!(target_os = "macos") {
        // in case CC=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/cc
        // we can not extract right path to frameworks, so add this
        extend_unique(
            &mut framework_dirs,
            [PathBuf::from(
                "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks",
            )],
        );
    }

    Ok((include_dirs, framework_dirs))
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
fn cc_system_include_dirs() -> Result<(Vec<PathBuf>, Vec<PathBuf>), Box<dyn std::error::Error>> {
    Ok((vec![], vec![]))
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

fn getenv_unwrap(v: &str) -> String {
    match env::var(v) {
        Ok(s) => s,
        Err(..) => fail(&format!("environment variable `{}` not defined", v)),
    }
}

fn fail(s: &str) -> ! {
    panic!("\n{}\n\nbuild script failed, must exit now", s)
}
