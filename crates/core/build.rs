// Links the Moonshine runtime built by scripts/build-moonshine.ps1 (Windows) or .sh (Linux, macOS)
// against the ONNX Runtime that sherpa-onnx ships (one runtime for every model). On Windows it also
// puts the CUDA DLLs next to the binaries, as hard links since they are gigabytes.
use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// Moonshine's static libraries and the CMake folder each is built in, each before the libraries it
/// uses, since GNU ld resolves archives in order.
const MOONSHINE_LIBS: [(&str, &str); 7] = [
    ("", "moonshine"),
    ("moonshine-tts", "moonshine_tts_ort"),
    ("moonshine-tts", "moonshine_tts_core"),
    ("moonshine-tts", "moonshine_tts_utf8proc"),
    ("bin-tokenizer", "bin-tokenizer"),
    ("ort-utils", "ort-utils"),
    ("moonshine-utils", "moonshine-utils"),
];

fn main() {
    // third_party/ sits at the workspace root, two folders up.
    let third_party = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../third_party");
    let build = third_party.join("moonshine-build");
    let runtime = third_party.join("sherpa-onnx/lib");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let windows = os == "windows";

    // Visual Studio builds put each library in a Release folder.
    let config = if windows { "Release" } else { "" };
    let moonshine = build.join(config).join(if windows { "moonshine.lib" } else { "libmoonshine.a" });
    let script = if windows { "ps1" } else { "sh" };
    if !moonshine.exists() {
        panic!("Moonshine is not built yet: run scripts/build-moonshine.{script}");
    }
    for (dir, lib) in MOONSHINE_LIBS {
        println!("cargo:rustc-link-search=native={}", build.join(dir).join(config).display());
        println!("cargo:rustc-link-lib=static={lib}");
    }
    println!("cargo:rerun-if-changed={}", moonshine.display());

    let onnxruntime = match os.as_str() {
        "windows" => "onnxruntime.dll",
        "macos" => "libonnxruntime.dylib",
        _ => "libonnxruntime.so",
    };
    if !runtime.join(onnxruntime).exists() {
        panic!("the runtime is not fetched yet: sherpa-onnx's libraries go in third_party/sherpa-onnx/lib (scripts/fetch-runtime.ps1 on Windows)");
    }
    println!("cargo:rustc-link-search=native={}", runtime.display());
    println!("cargo:rustc-link-lib=dylib=onnxruntime");
    match os.as_str() {
        // sherpa-onnx-sys copies the runtime's own DLLs; linking them too would make its copy fail.
        "windows" => place_dlls(&third_party.join("cuda-runtime")),
        // Moonshine is C++; sherpa-onnx-sys copies the runtime next to the binaries, found through the rpath.
        "macos" => {
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
        }
        _ => {
            println!("cargo:rustc-link-lib=dylib=stdc++");
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        }
    }
}

/// Hard-links every DLL in `folder` next to the binaries and examples.
fn place_dlls(folder: &Path) {
    // OUT_DIR is target/<profile>/build/<pkg>/out; binaries live in target/<profile>/.
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile_dir = out.ancestors().nth(3).unwrap();
    let dlls: Vec<PathBuf> = fs::read_dir(folder)
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "dll"))
        .collect();
    for dir in [profile_dir.to_path_buf(), profile_dir.join("examples")] {
        fs::create_dir_all(&dir).unwrap();
        for dll in &dlls {
            let target = dir.join(dll.file_name().unwrap());
            let _ = fs::remove_file(&target);
            fs::hard_link(dll, &target).or_else(|_| fs::copy(dll, &target).map(|_| ())).unwrap();
        }
    }
}
