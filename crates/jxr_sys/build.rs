fn main() {
    let v = "vendor";
    let files = [
        "image/sys/adapthuff.c",
        "image/sys/image.c",
        "image/sys/strcodec.c",
        "image/sys/strPredQuant.c",
        "image/sys/strTransform.c",
        "image/sys/perfTimerANSI.c",
        "image/decode/decode.c",
        "image/decode/postprocess.c",
        "image/decode/segdec.c",
        "image/decode/strdec.c",
        "image/decode/strInvTransform.c",
        "image/decode/strPredQuantDec.c",
        "image/decode/JXRTranscode.c",
    ];
    let mut b = cc::Build::new();
    for f in files {
        b.file(format!("{v}/{f}"));
        println!("cargo:rerun-if-changed={v}/{f}");
    }
    b.file("src/shim.c")
        .include(v)
        .include(format!("{v}/common/include"))
        .include(format!("{v}/image/sys"))
        .include(format!("{v}/image/decode"))
        .define("__ANSI__", None)
        .define("DISABLE_PERF_MEASUREMENT", None)
        .warnings(false)
        .compile("jxr");
    println!("cargo:rerun-if-changed=src/shim.c");
}
