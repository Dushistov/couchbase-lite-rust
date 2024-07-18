use bindgen::{callbacks::ParseCallbacks, Builder, RustTarget};
use quote::ToTokens;
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    str,
    sync::{Arc, Mutex},
};

#[cfg(feature = "docs-rs")]
fn main() {}

#[cfg(not(feature = "docs-rs"))]
fn main() {
    env_logger::init();
    let target = getenv_unwrap("TARGET");
    let target_os = getenv_unwrap("CARGO_CFG_TARGET_OS");
    let is_msvc = target.contains("msvc");

    if cfg!(feature = "with-asan") && !cfg!(feature = "build") {
        panic!("Invalid set of options: with-asan should be used with build");
    }

    let sdir = download_source_code_via_git_if_needed().expect("download of source code failed");

    let bdirs = cmake_build_src_dir(&sdir, is_msvc);
    println!("build directory: {bdirs:?}\nsource directory {sdir:?}");

    let native_libs_search_paths = if bdirs.is_empty() {
        panic!("You didn't specify build directory for couchbase-lite-core");
    } else if bdirs.len() == 1 {
        specify_library_search_dirs_for_std_layout(&bdirs[0])
    } else {
        bdirs
    };
    for path in &native_libs_search_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }

    if cfg!(feature = "use-couchbase-lite-sqlite") {
        println!("cargo:rustc-link-lib=static=CouchbaseSqlite3");
    }
    if cfg!(feature = "use-couchbase-lite-websocket") {
        println!("cargo:rustc-link-lib=static=LiteCoreWebSocket");
    }
    for lib in [
        "LiteCoreStatic",
        "FleeceStatic",
        "SQLite3_UnicodeSN",
        "BLIPStatic",
        "mbedcrypto",
        "mbedtls",
        "mbedx509",
    ] {
        println!("cargo:rustc-link-lib=static={lib}");
        match find_full_library_path(&native_libs_search_paths, lib) {
            Ok(full_path) => {
                println!("cargo:rerun-if-changed={}", full_path.display());
            }
            Err(err) => {
                panic!("{err}");
            }
        }
    }

    if let Ok(icu_lib_path) = env::var("ICU_LIB_DIR") {
        println!("cargo:rustc-link-search=native={icu_lib_path}");
        println!("cargo:rerun-if-env-changed=ICU_LIB_DIR");
    }

    if target_os == "linux" {
        println!("cargo:rustc-link-lib=icuuc");
        println!("cargo:rustc-link-lib=icui18n");
        println!("cargo:rustc-link-lib=icudata");
        println!("cargo:rustc-link-lib=z");
        println!("cargo:rustc-link-lib=stdc++");
    } else if target_os == "macos" || target_os == "ios" {
        println!("cargo:rustc-link-lib=z");
        //TODO: remove this dependicies: CoreFoundation + Foundation
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=SystemConfiguration");
        println!("cargo:rustc-link-lib=framework=Security");
        println!("cargo:rustc-link-lib=c++");
    } else if target_os == "android" {
        println!("cargo:rustc-link-lib=icuuc");
        println!("cargo:rustc-link-lib=icui18n");
        println!("cargo:rustc-link-lib=z");
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

fn find_full_library_path(
    native_libs_search_paths: &[PathBuf],
    lib: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let static_lib_ext = if win_target() { "lib" } else { "a" };
    let prefix = "lib";
    let file_name = format!("{prefix}{lib}.{static_lib_ext}");

    for path in native_libs_search_paths {
        let full_path = path.join(&file_name);
        if full_path.exists() {
            return Ok(full_path);
        }
    }
    Err(format!("Can no find {file_name} in {native_libs_search_paths:?}").into())
}

fn specify_library_search_dirs_for_std_layout(bdir: &Path) -> Vec<PathBuf> {
    vec![
        bdir.to_path_buf(),
        bdir.join("vendor").join("fleece"),
        bdir.join("Networking").join("BLIP"),
        bdir.join("vendor").join("sqlite3-unicodesn"),
        bdir.join("vendor")
            .join("mbedtls")
            .join("crypto")
            .join("library"),
        bdir.join("vendor").join("mbedtls").join("library"),
    ]
}

#[cfg(feature = "git-download")]
fn download_source_code_via_git_if_needed() -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::process::Command;
    use which::which;

    const URL: &str = "https://github.com/Dushistov/couchbase-lite-core";
    const COMMIT_SHA1: &str = "b963be478a9b97fd149326dc69581f6733b23c23";

    let git_path = which("git")?;
    let cur_dir = env::current_dir()?;
    let parent_dir = cur_dir
        .parent()
        .ok_or_else(|| format!("Can not find parent directory for current({cur_dir:?})"))?;
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

    Ok(src_dir)
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
            .map_err(|_| format!("Can not find {header}"))?;
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
    let collect_includes = CollectIncludes::default();
    let couchbase_includes = collect_includes.0.clone();
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
        .rustified_enum("FLValueType")
        .rustified_enum("FLTrust")
        .no_copy("FLSliceResult")
        // we not use string_view, and there is bindgen's bug:
        // https://github.com/rust-lang/rust-bindgen/issues/2152
        .layout_tests(false)
        .parse_callbacks(Box::new(collect_includes));

    bindings = include_dirs.iter().fold(bindings, |acc, x| {
        acc.clang_arg("-I".to_string() + x.as_ref().to_str().unwrap())
    });
    bindings = framework_dirs.iter().fold(bindings, |acc, x| {
        acc.clang_arg("-F".to_string() + x.as_ref().to_str().unwrap())
    });

    bindings = bindings
        .rust_target(RustTarget::Stable_1_47)
        .opaque_type("timex")//to big reserved part for Debug
        .blocklist_type("max_align_t")//long double not supported,
                                      // see https://github.com/rust-lang/rust-bindgen/issues/550
        ;
    bindings = if target.contains("windows") {
        //see https://github.com/servo/rust-bindgen/issues/578
        bindings.trust_clang_mangling(false)
    } else {
        bindings
    };

    bindings = c_headers[1..]
        .iter()
        .try_fold(bindings, |acc: Builder, header| {
            let c_file_path = search_file_in_directory(include_dirs, header)
                .map_err(|_| format!("Can not find {header}"))?;
            let c_file_str = c_file_path
                .to_str()
                .ok_or_else(|| format!("Invalid unicode in path to {header}"))?;
            Ok::<_, String>(acc.clang_arg("-include").clang_arg(c_file_str))
        })?;
    let generated_bindings = bindings
        .generate()
        .map_err(|_| "Failed to generate bindings".to_string())?;
    let mut rust_code = Vec::new();
    generated_bindings
        .write(Box::new(&mut rust_code))
        .map_err(|err| err.to_string())?;
    let couchbase_includes = couchbase_includes.lock().unwrap();
    let rust_code = str::from_utf8(&rust_code)
        .map_err(|err| format!("Bindgen gerated code is not valid utf-8: {err}"))?;
    let mod_rust_code = handle_c4_enum_option(rust_code, &couchbase_includes)?;
    fs::write(output_rust, &mod_rust_code).map_err(|err| err.to_string())?;
    Ok(())
}

fn handle_c4_enum_option(code: &str, couchbase_includes: &[String]) -> Result<Vec<u8>, String> {
    let (c4_enum_names, c4_opt_names) = find_all_c4_enum_option(couchbase_includes)?;

    let mut ret = Vec::with_capacity(code.as_bytes().len());
    let ast = syn::parse_file(code)
        .map_err(|err| format!("syn failed to parse generated by bindgen code: {err}"))?;
    let mut it = ast.items.iter();
    while let Some(item) = it.next() {
        if let syn::Item::Type(syn::ItemType {
            vis: syn::Visibility::Public(..),
            ident,
            ty: enum_repr_ty,
            ..
        }) = item
        {
            if let Some((is_enum, pos)) =
                find_indent_in_enum_or_opt(&ident, &c4_enum_names, &c4_opt_names)
            {
                let enum_name = if is_enum {
                    &c4_enum_names[pos]
                } else {
                    &c4_opt_names[pos]
                };
                let enum_repr_ty = enum_repr_ty.into_token_stream().to_string();

                let enum_handling =
                    if enum_name == "C4QueryLanguage" || enum_name == "C4ReplicatorActivityLevel" {
                        EnumHandling::Rust
                    } else {
                        EnumHandling::NewType
                    };
                match enum_handling {
                    EnumHandling::NewType => {
                        writeln!(
                            ret,
                            r#"
    #[repr(transparent)]
    #[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
    pub struct {enum_name}(pub {enum_repr_ty});"#
                        )
                        .unwrap();
                        writeln!(ret, "impl {enum_name} {{").unwrap();
                    }
                    EnumHandling::Rust => {
                        writeln!(
                            ret,
                            r#"
    #[repr({enum_repr_ty})]
    #[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
    pub enum {enum_name} {{"#
                        )
                        .unwrap();
                    }
                }

                let mut ty_name: Option<String> = None;
                for item in it.by_ref() {
                    match item {
                        syn::Item::Const(syn::ItemConst {
                            vis: syn::Visibility::Public(..),
                            ident: var_name,
                            ty: var_ty,
                            expr: var_val,
                            ..
                        }) => {
                            let cur_ty_name = var_ty.into_token_stream().to_string();
                            if let Some(ty_name) = ty_name {
                                if ty_name != cur_ty_name {
                                    return Err(format!(
                                        "Invalid variant value type, expect type {ty_name} in {}",
                                        item.into_token_stream()
                                    ));
                                }
                            }
                            ty_name = Some(cur_ty_name);
                            let var_val = var_val.into_token_stream().to_string();
                            match enum_handling {
                                EnumHandling::NewType => writeln!(
                                ret,
                                "    pub const {var_name}: {enum_name} = {enum_name}({var_val});"
                            )
                                .unwrap(),
                                EnumHandling::Rust => {
                                    writeln!(ret, "    {var_name} = {var_val},").unwrap()
                                }
                            }
                        }
                        syn::Item::Type(syn::ItemType {
                            vis: syn::Visibility::Public(..),
                            ident,
                            ..
                        }) => {
                            if ty_name.as_ref().map(|s| *ident != s).unwrap_or(false) {
                                return Err(format!(
                                    "Invalid variant value type, expect type {ty_name:?} != {ident} in {}",
                                    item.into_token_stream()
                                ));
                            }
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "Unexpected line, no pub const or pub type: {}",
                                item.into_token_stream()
                            ))
                        }
                    }
                }
                ret.extend_from_slice(b"}\n");

                if !is_enum {
                    writeln!(
                        ret,
                        r#"impl std::ops::BitAnd for {enum_name} {{
        type Output = Self;
        #[doc = " Returns the intersection between the two sets of flags."]
        #[inline]
        fn bitand(self, other: Self) -> Self {{
            Self(self.0 & other.0)
        }}
    }}

    impl std::ops::BitOr for {enum_name} {{
        type Output = Self;
        #[doc = " Returns the union of the two sets of flags."]
        #[inline]
        fn bitor(self, other: Self) -> Self {{
            Self(self.0 | other.0)
        }}
    }}"#
                    )
                    .unwrap();
                }

                continue;
            }
        }
        ret.extend_from_slice(item.into_token_stream().to_string().as_bytes());
        ret.push(b'\n');
    }

    Ok(ret)
}

fn find_indent_in_enum_or_opt(
    ident: &&syn::Ident,
    c4_enum: &[String],
    c4_opt: &[String],
) -> Option<(bool, usize)> {
    if let Some(pos) = c4_enum.iter().position(|n| *ident == n.as_str()) {
        return Some((true, pos));
    }
    if let Some(pos) = c4_opt.iter().position(|n| *ident == n.as_str()) {
        return Some((false, pos));
    }
    None
}

enum EnumHandling {
    NewType,
    Rust,
}

fn find_all_c4_enum_option(
    couchbase_includes: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut c4_enum_names = Vec::new();
    let mut c4_opt_names = Vec::new();
    for c_include in couchbase_includes {
        let file =
            fs::File::open(c_include).map_err(|err| format!("Can not open {c_include}: {err}"))?;
        let file = BufReader::new(file);
        for line in file.lines() {
            let line = line.map_err(|err| format!("Error during read from {c_include}: {err}"))?;
            if line.starts_with("//")
                || line.contains("C4_ENUM_ATTRIBUTES")
                || contains_define_of_enum_with_name(&line, "C4_ENUM")
                || contains_define_of_enum_with_name(&line, "C4_OPTIONS")
                || line.contains("C4_OPTIONS_ATTRIBUTES")
                || line.contains("__C4_ENUM_##_name")
                || line.contains("__C4_OPTIONS_##_name")
            {
                continue;
            }
            if let Some(pos) = line.find("C4_ENUM") {
                let name = extract_name_from_c4_macro("C4_ENUM", pos, &line)?;
                println!("Found C4_ENUM {name}");
                c4_enum_names.push(name);
            }
            if let Some(pos) = line.find("C4_OPTIONS") {
                let name = extract_name_from_c4_macro("C4_OPTIONS", pos, &line)?;
                println!("Found C4_OPTIONS {name}");
                c4_opt_names.push(name);
            }
        }
    }
    Ok((c4_enum_names, c4_opt_names))
}

fn contains_define_of_enum_with_name(line: &str, enum_name: &str) -> bool {
    let Some(pos) = line.find(|ch: char| !ch.is_whitespace()) else {
        return false;
    };

    if !line[pos..].starts_with('#') {
        return false;
    }
    let line = &line[pos + 1..];

    let Some(pos) = line.find(|ch: char| !ch.is_whitespace()) else {
        return false;
    };
    const DEFINE: &str = "define";
    if !line[pos..].starts_with(DEFINE) {
        return false;
    }
    let line = &line[pos + DEFINE.len()..];
    let Some(pos) = line.find(|ch: char| !ch.is_whitespace()) else {
        return false;
    };
    if !line[pos..].starts_with(enum_name) {
        return false;
    }
    let line = &line[pos + enum_name.len()..];
    line.starts_with(' ')
}

fn extract_name_from_c4_macro(
    keyword: &str,
    start_pos: usize,
    line: &str,
) -> Result<String, String> {
    let rest = &line[start_pos + keyword.len()..];
    let mut it = rest.chars();
    if !matches!(it.next(), Some('(')) {
        return Err(format!("Expect '(' after {keyword}: '{line}'"));
    }
    let mut found_comma = false;
    for ch in it.by_ref() {
        if ch == ',' {
            found_comma = true;
            break;
        }
    }
    if !found_comma {
        return Err(format!("No ',' after '(' in {line}"));
    }
    let mut ret = String::new();
    for ch in it.by_ref() {
        if !ch.is_whitespace() {
            ret.push(ch);
            break;
        }
    }
    if ret.is_empty() {
        return Err(format!("No not whitespaces after ',' in {line}"));
    }
    let mut found_close_bracket = false;
    for ch in it {
        if ch != ')' {
            ret.push(ch);
        } else {
            found_close_bracket = true;
            break;
        }
    }

    if !found_close_bracket {
        return Err(format!("No ')' in {line}"));
    }

    Ok(ret)
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

    cmake_config.build_arg("LiteCoreStatic");
    cmake_config.build_arg("FleeceStatic");
    cmake_config.build_arg("BLIPStatic");
    if cfg!(feature = "use-couchbase-lite-websocket") {
        cmake_config.build_arg("LiteCoreWebSocket");
    }
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
    const DIRS_VAR: &str = "COUCHBASE_LITE_CORE_BUILD_DIRS";
    const DIR_VAR: &str = "COUCHBASE_LITE_CORE_BUILD_DIR";
    if let Ok(dirs) = env::var(DIRS_VAR) {
        if env::var(DIR_VAR).is_ok() {
            panic!("Error: {DIR_VAR} and {DIRS_VAR} are setted at the same time, should be only one of them");
        }
        println!("cargo:rerun-if-env-changed={DIRS_VAR}");
        let mut ret = vec![];
        for d in dirs.split('^') {
            let d: PathBuf = d.into();
            if !ret.iter().any(|e| *e == d) {
                ret.push(d);
            }
        }
        return ret;
    }
    println!("cargo:rerun-if-env-changed={DIR_VAR}");
    vec![getenv_unwrap(DIR_VAR).into()]
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn cc_system_include_dirs() -> Result<(Vec<PathBuf>, Vec<PathBuf>), Box<dyn std::error::Error>> {
    use std::{
        io::Read,
        process::{Command, Stdio},
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

    fn contains_subslice<T: PartialEq>(data: &[T], needle: &[T]) -> bool {
        data.windows(needle.len()).any(|w| w == needle)
    }

    if getenv_unwrap("CARGO_CFG_TARGET_OS") == "ios" {
        let output = Command::new("clang").arg("--version").output()?;

        if contains_subslice(&output.stdout, b"Apple clang version 12.0.0") {
            println!("Using too old apple compiler, which can not handle SDKROOT");
            std::env::remove_var("SDKROOT");
        }
    }

    for lang in &["c", "c++"] {
        let cc_build = cc::Build::new();

        let cc_process = cc_build
            .get_compiler()
            .to_command()
            .env("LANG", "C")
            .env("LC_MESSAGES", "C")
            .args(["-v", "-x", lang, "-E", "-"])
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
            .ok_or_else(|| format!("No '{BEGIN_PAT}' in output from cc"))?
            + BEGIN_PAT.len();
        let end_includes = cc_output[start_includes..]
            .find(END_PAT)
            .ok_or_else(|| format!("No '{END_PAT}' in output from cc"))?
            + start_includes;

        const FRAMEWORK_PAT: &str = " (framework directory)";

        extend_unique(
            &mut include_dirs,
            cc_output[start_includes..end_includes]
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
            cc_output[start_includes..end_includes]
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
        Err(..) => fail(&format!("environment variable `{v}` not defined")),
    }
}

fn fail(s: &str) -> ! {
    panic!("\n{s}\n\nbuild script failed, must exit now")
}

#[derive(Debug, Default)]
struct CollectIncludes(Arc<Mutex<Vec<String>>>);

impl ParseCallbacks for CollectIncludes {
    fn include_file(&self, filename: &str) {
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed
        println!("cargo:rerun-if-changed={}", filename);
        self.0.lock().unwrap().push(filename.into());
    }
}

/// Tells whether we're building for Windows. This is more suitable than a plain
/// `cfg!(windows)`, since the latter does not properly handle cross-compilation
///
/// Note that there is no way to know at compile-time which system we'll be
/// targeting, and this test must be made at run-time (of the build script) See
/// https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
fn win_target() -> bool {
    env::var("CARGO_CFG_WINDOWS").is_ok()
}
