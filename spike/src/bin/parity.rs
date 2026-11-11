// 判据 5 的 LLRT 侧 runner：把一份 ESM 源喂进内嵌 runtime，驱动到 idle。
// 用法：parity <file.mjs>
use spike::engine::{assemble, global_inits, run_module};

#[tokio::main]
async fn main() {
    global_inits();
    let path = std::env::args().nth(1).expect("usage: parity <file.mjs>");
    let src = std::fs::read_to_string(&path).expect("read probe source");

    let (rt, ctx) = assemble().await.expect("assemble");
    if let Err(e) = run_module(&ctx, &rt, &src).await {
        eprintln!("parity eval failed: {e:?}");
        std::process::exit(1);
    }
}