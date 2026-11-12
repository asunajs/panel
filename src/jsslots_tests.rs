//! 全局闸的边界：名额不能是 0，且真的会挡住并发的第三段 JS。

use super::*;

#[test]
fn the_limit_always_leaves_room_for_the_server() {
    let n = limit();
    assert!(n >= 1, "名额不能是 0，否则什么都跑不了");
    let cores = std::thread::available_parallelism()
        .map(|c| c.get())
        .unwrap_or(0);
    if cores > 2 {
        assert_eq!(n, cores - 2, "要留 2 个 worker 给 HTTP / 调度 / DB actor");
    }
}

#[tokio::test]
async fn permits_are_finite_and_returned_on_drop() {
    let n = limit();
    // 全部拿满
    let mut held = Vec::new();
    for _ in 0..n {
        held.push(try_acquire().expect("还没拿满就该拿得到"));
    }
    assert!(try_acquire().is_none(), "拿满之后不该再拿得到");
    assert_eq!(available(), 0);
    // 放回去一个
    held.pop();
    assert_eq!(available(), 1, "drop 之后名额要回来");
    assert!(try_acquire().is_some());
}
