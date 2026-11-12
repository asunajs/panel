//! 熔断与推进的回归。全是纯状态机，不需要引擎/DB。

use super::*;

fn task(cron: &str) -> Task {
    Task::new("p".into(), "t".into(), cron.into(), DEFAULT_BUDGET_MS).expect("parse")
}

fn cfg() -> TripConfig {
    TripConfig {
        threshold: 3,
        base_cooldown: Duration::from_secs(10),
        max_cooldown: Duration::from_secs(80),
    }
}

fn fail(t: &mut Task, c: &TripConfig, now: DateTime<Local>) {
    t.advance(c, now, Some("boom".into()));
}

#[test]
fn cooldown_is_exponential_and_capped() {
    let c = cfg();
    let ms = |n| cooldown_for(&c, n).num_milliseconds();
    assert_eq!(ms(1), 10_000);
    assert_eq!(ms(2), 20_000);
    assert_eq!(ms(3), 40_000);
    assert_eq!(ms(4), 80_000);
    assert_eq!(ms(5), 80_000, "封顶");
    // 移位不能溢出
    assert_eq!(ms(u32::MAX), 80_000);
}

#[test]
fn trips_only_after_threshold_consecutive_failures() {
    let c = cfg();
    let now = Local::now();
    let mut t = task("@every 1s");
    fail(&mut t, &c, now);
    fail(&mut t, &c, now);
    assert!(!t.tripped, "两次还没到 threshold=3");
    fail(&mut t, &c, now);
    assert!(t.tripped);
    assert_eq!(t.trips, 1);
    assert_eq!(t.next_fire, now + ChronoDuration::seconds(10));
}

#[test]
fn a_success_clears_the_failure_streak() {
    let c = cfg();
    let now = Local::now();
    let mut t = task("@every 1s");
    fail(&mut t, &c, now);
    fail(&mut t, &c, now);
    t.advance(&c, now, None);
    assert_eq!(t.consecutive_failures, 0);
    fail(&mut t, &c, now);
    fail(&mut t, &c, now);
    assert!(!t.tripped, "成功把连击清零了，不该攒到 3");
}

#[test]
fn half_open_success_fully_recovers() {
    let c = cfg();
    let now = Local::now();
    let mut t = task("@every 1s");
    for _ in 0..3 {
        fail(&mut t, &c, now);
    }
    assert!(t.tripped);
    let retry_at = t.next_fire;
    t.advance(&c, retry_at, None);
    assert!(!t.tripped);
    assert_eq!(t.trips, 0, "退避计数清零");
    assert!(t.next_fire > retry_at, "恢复后回到正常节奏");
    assert_eq!(t.consecutive_failures, 0);
}

#[test]
fn half_open_failure_doubles_the_cooldown() {
    let c = cfg();
    let now = Local::now();
    let mut t = task("@every 1s");
    for _ in 0..3 {
        fail(&mut t, &c, now);
    }
    let first = t.next_fire;
    // 半开只需再失败**一次**就继续熔断，不用重新攒 threshold
    fail(&mut t, &c, first);
    assert!(t.tripped);
    assert_eq!(t.trips, 2);
    assert_eq!(t.next_fire, first + ChronoDuration::seconds(20));
}

#[test]
fn normal_advance_does_not_drift_with_execution_time() {
    let c = cfg();
    let mut t = task("@every 1s");
    let start = t.base_fire;
    // 第 i 拍在 start+(i-1)*1000 触发，耗时 300ms 才结算。
    // 「现在」必须落在那一拍之后、下一拍之前，否则走的是积压重对齐那条路。
    for i in 1..=5 {
        let late = start + ChronoDuration::milliseconds((i - 1) * 1000 + 300);
        t.advance(&c, late, None);
        assert_eq!(
            t.base_fire,
            start + ChronoDuration::milliseconds(i * 1000),
            "第 {i} 拍后理论线漂移了"
        );
    }
}

#[test]
fn falling_behind_realigns_instead_of_replaying_backlog() {
    let c = cfg();
    let mut t = task("@every 1s");
    let now = t.base_fire + ChronoDuration::hours(2);
    t.advance(&c, now, None);
    assert!(t.next_fire > now, "落后两小时只跳拍，不补跑 7200 次");
    assert!(t.next_fire <= now + ChronoDuration::seconds(1));
}

#[test]
fn twenty_minute_budget_is_allowed_and_cap_is_ninety_minutes() {
    let twenty_min = 20 * 60 * 1000;
    let t = Task::new("p".into(), "t".into(), "@every 1h".into(), twenty_min).unwrap();
    assert_eq!(t.budget_ms, twenty_min, "20 分钟不能被钳掉");
    assert!(MAX_BUDGET_MS >= 5_400_000, "上限至少 90 分钟");
    let over = Task::new("p".into(), "t".into(), "@every 1h".into(), u64::MAX).unwrap();
    assert_eq!(over.budget_ms, MAX_BUDGET_MS);
    let zero = Task::new("p".into(), "t".into(), "@every 1h".into(), 0).unwrap();
    assert_eq!(zero.budget_ms, 1, "0 会让每拍立刻超预算");
}

#[test]
fn invalid_expression_is_rejected_at_registration() {
    assert!(Task::new("p".into(), "t".into(), "not a cron".into(), 1000).is_err());
    assert!(Task::new("p".into(), "t".into(), "".into(), 1000).is_err());
}

#[test]
fn jitter_does_not_accumulate_on_the_theoretical_line() {
    let c = cfg();
    let mut t = task("@every 10s -2s");
    let start = t.base_fire;
    for i in 1..=20 {
        // 结算时刻取「那一拍之后 1s」：负偏移最多 2s，所以 actual 仍在 now 之后，
        // 走的是正常推进而不是重对齐 —— 否则这条用例测不到抖动累积。
        let now = start + ChronoDuration::milliseconds((i - 1) * 10_000 + 1000);
        t.advance(&c, now, None);
        assert_eq!(
            t.base_fire,
            start + ChronoDuration::milliseconds(i * 10_000),
            "抖动跑到理论线上去了，第 {i} 拍"
        );
        let offset = (t.base_fire - t.next_fire).num_milliseconds();
        assert!((0..=2000).contains(&offset), "负偏移越界: {offset}");
    }
}

#[test]
fn manual_tasks_never_become_due_and_never_trip() {
    let c = cfg();
    let mut t = Task::manual("p".into(), "only_handler".into());
    assert!(t.schedule.is_none());
    assert!(t.cron.is_empty());
    let now = Local::now();
    for _ in 0..10 {
        fail(&mut t, &c, now);
    }
    assert!(!t.tripped, "手动任务由人触发，不该被熔断锁住");
    assert_eq!(t.runs, 10);
    assert!(t.next_fire > now + ChronoDuration::days(300), "手动任务不该有到期时刻");
}

#[tokio::test]
async fn catch_engine_panic_passes_ok_and_err_through() {
    assert_eq!(catch_engine_panic(async { Ok(()) }).await, Ok(()));
    assert_eq!(
        catch_engine_panic::<(), _>(async { Err("boom".to_string()) }).await,
        Err("boom".to_string())
    );
}

#[tokio::test]
async fn catch_engine_panic_turns_a_panic_into_a_failed_run() {
    // &str 与 String 两种 payload 都要能取出文本；否则 busy 会卡死（见 helper 文档）。
    let e = catch_engine_panic::<(), _>(async { panic!("kaboom") }).await.unwrap_err();
    assert!(e.contains("引擎 panic"), "实际: {e}");
    assert!(e.contains("kaboom"), "实际: {e}");

    let e = catch_engine_panic::<(), _>(async { panic!("{}", "owned".to_string()) })
        .await
        .unwrap_err();
    assert!(e.contains("owned"), "实际: {e}");
}
// ---------------------------------------------------------------- 提前唤醒（lead_ms）
//
// `lead_ms` 是「提前多少毫秒唤醒」。语义：派发时刻 = 计划时刻 - lead_ms，事件里
// `early=true` + `lead_ms=N`，插件据此知道「还没到点，先做准备」。
// 这几条把「提前量算得对」「准点的不算早」「超上限要报错」钉住。

#[test]
fn trigger_time_is_the_plan_minus_the_lead() {
    let mut t = task("@every 1h");
    t.next_fire = Local::now() + ChronoDuration::seconds(100);
    let plan = t.next_fire;

    // 没有提前量：准点
    t.lead_ms = 0;
    assert_eq!(trigger_at(&t), plan, "lead_ms=0 就该准点");

    // 提前 30 秒：触发时刻早 30 秒，计划时刻本身不变
    t.lead_ms = 30_000;
    assert_eq!(
        trigger_at(&t),
        plan - ChronoDuration::milliseconds(30_000),
        "提前 30 秒唤醒"
    );
    assert_eq!(t.next_fire, plan, "next_fire（计划时刻）不该被 lead 改掉");
}

#[test]
fn a_task_with_a_lead_is_due_before_its_plan_time() {
    // 这就是「提前唤醒」的全部含义：now 还没到 next_fire，但已经到 trigger_at 了。
    let mut t = task("@every 1h");
    let now = Local::now();
    t.next_fire = now + ChronoDuration::seconds(10);
    t.lead_ms = 20_000;
    assert!(trigger_at(&t) <= now, "提前 20 秒的任务，now 就该到期了");
    assert!(t.next_fire > now, "但计划时刻还没到 —— 事件里 early 才为 true");

    // 反过来：提前量不够，就不该到期
    t.lead_ms = 5_000;
    assert!(trigger_at(&t) > now, "只提前 5 秒，还不到时候");
}

#[test]
fn the_lead_is_capped_when_computing_the_trigger_time() {
    // 防御性：就算库里存了一个超上限的值（旧数据/手改库），算触发时刻时也不能
    // 提前一天就把任务点着。写入侧已经在报错了，这里兜住读出来的脏数据。
    let mut t = task("@every 1h");
    t.next_fire = Local::now() + ChronoDuration::seconds(100);
    t.lead_ms = LEAD_MAX_MS * 10;
    let want = t.next_fire - ChronoDuration::milliseconds(LEAD_MAX_MS as i64);
    assert_eq!(trigger_at(&t), want, "算触发时刻时按上限裁剪");
}

#[test]
fn only_a_panel_set_lead_is_persisted() {
    // 插件注册给的默认值：不落库（下次注册以插件当时给的为准）
    assert_eq!(saved_lead(false, 4000), None, "注册值不该写库");
    assert_eq!(saved_lead(false, 0), None);
    // 面板改过的：落库（热重载/重启都要留住）
    assert_eq!(saved_lead(true, 2000), Some(2000));
    assert_eq!(saved_lead(true, 0), Some(0), "面板明确改成 0 也要留住");
}

// ---------------------------------------------------------------- 落后（该跑没跑）
//
// 12 个 `@every 5s` 挤 8 个 JS 名额：实测 4 个只跑了 3 次（本该 5 次），以前**一条 warn 都没有**。
// 用户看到的是「有的整点任务没跑」，无从下手。现在按任务 60 秒节流地报出来。

#[test]
fn behind_is_noted_at_once_then_at_most_every_minute() {
    let now = Local::now();
    assert!(should_note_behind(None, now), "第一次一定说");
    assert!(
        !should_note_behind(Some(now - ChronoDuration::seconds(30)), now),
        "30 秒内不重复刷屏"
    );
    assert!(should_note_behind(Some(now - ChronoDuration::seconds(61)), now));
    assert!(should_note_behind(Some(now - ChronoDuration::seconds(600)), now));
}

/// 落后计数的含义：`behind` 是「这一轮里攒了几拍」，报完清零、到 60 秒再报。
#[test]
fn behind_counter_accumulates_until_reported() {
    let now = Local::now();
    let mut t = task("@every 5s");
    t.next_fire = now - ChronoDuration::seconds(10); // 已经过点
    t.in_flight = true;

    assert_eq!(t.behind, 0);
    t.behind += 1;
    t.behind += 1;
    assert_eq!(t.behind, 2, "两拍没跑成就是 2");
    assert!(should_note_behind(t.behind_noted, now));
    t.behind_noted = Some(now);
    t.behind = 0;
    assert!(!should_note_behind(t.behind_noted, now), "刚报过就不再报");
}
