// Stage 0 criterion 1: does llrt_core build and run at all, using upstream's own Vm?
use llrt_core::vm::Vm;

#[tokio::main]
async fn main() {
    let vm = Vm::new().await.expect("Vm::new failed");
    vm.run(
        "globalThis.__probe = ['esm-ok', typeof fetch, typeof Buffer, typeof structuredClone, typeof require].join(' | ');",
        true,
        true,
    )
    .await;
    vm.run("console.log(__probe);", true, true).await;
    vm.idle().await.expect("idle failed");
}