//! Drives the real `catapulte __render-sandbox` child, plus scripted fake children for the
//! protocol edge cases. Serial: several tests count the children of this process.

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use catapulte::render_sandbox::{RenderSandbox, RenderSandboxConfig};
use catapulte_domain::entity::body::{InterpolatedBody, Plain, ResolvedBody};
use catapulte_domain::port::template_interpolator::{InterpolateError, TemplateInterpolator};
use catapulte_domain::port::template_renderer::{RenderError, TemplateRenderer};
use catapulte_domain::port::template_validator::TemplateValidator;
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::include_loader::IncludeLoaderConfig;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use serde_json::{Map, Value};
use serial_test::serial;

const BIN: &str = env!("CARGO_BIN_EXE_catapulte");

type Variables = Map<String, Value>;

fn config() -> RenderSandboxConfig {
    RenderSandboxConfig {
        concurrency: 2,
        ..RenderSandboxConfig::new(BIN.into())
    }
}

fn sandbox() -> RenderSandbox {
    RenderSandbox::new(config(), IncludeLoaderConfig::default())
}

fn sandbox_with(config: RenderSandboxConfig) -> RenderSandbox {
    RenderSandbox::new(config, IncludeLoaderConfig::default())
}

fn vars(pairs: &[(&str, Value)]) -> Variables {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn mjml(source: &str) -> ResolvedBody {
    ResolvedBody::Mjml(source.to_owned())
}

fn doubled(name: &str, times: usize) -> String {
    format!(
        "{{% set {name} = ['x'] %}}{}",
        format!("{{% set {name} = [{name}, {name}] %}}").repeat(times)
    )
}

/// `~` on a doubled list: exponential time, little memory.
fn tilde_template() -> String {
    format!("{}{{{{ a ~ 'y' }}}}", doubled("a", 40))
}

/// `==` on two separately built towers.
fn eq_template() -> String {
    let mut source = String::from("{% set a = ['x'] %}{% set b = ['x'] %}");
    source.push_str(&"{% set a = [a, a] %}{% set b = [b, b] %}".repeat(28));
    source.push_str("{{ a == b }}");
    source
}

/// A repeated `a * 1000` list formatted in one call.
const REPEATED_LIST: &str = "{% set a = ['x'] * 1000 %}{% set b = a * 1000 %}{{ b }}";

const MJML_DOC: &str = "<mjml><mj-head><mj-preview>Hi {{ n }}</mj-preview></mj-head><mj-body><mj-section><mj-column><mj-text>{{ n }}</mj-text></mj-column></mj-section></mj-body></mjml>";

/// Children of this process, from the kernel's per-thread lists.
fn children() -> Vec<u32> {
    let mut found = Vec::new();
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let path = task.path().join("children");
        if let Ok(text) = std::fs::read_to_string(path) {
            found.extend(
                text.split_whitespace()
                    .filter_map(|p| p.parse::<u32>().ok()),
            );
        }
    }
    found
}

async fn wait_for_no_children() {
    let started = Instant::now();
    while !children().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "children left: {:?}",
            children()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// An executable shell script standing in for the child.
struct Script {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn script(body: &str) -> Script {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Script { _dir: dir, path }
}

fn scripted(script: &Script, timeout: Duration) -> RenderSandbox {
    sandbox_with(RenderSandboxConfig {
        timeout,
        ..RenderSandboxConfig::new(script.path.clone())
    })
}

async fn validate(sandbox: &RenderSandbox, source: &str) -> Result<String, InterpolateError> {
    sandbox.validate(source, &Variables::new()).await
}

fn is_budget(result: &Result<String, InterpolateError>) -> bool {
    matches!(result, Err(InterpolateError::BudgetExceeded))
}

fn is_infra(result: &Result<String, InterpolateError>) -> bool {
    matches!(result, Err(InterpolateError::Infrastructure { .. }))
}

// ---- hostile templates ------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn tilde_on_a_doubled_list_dies_by_the_cpu_limit_within_the_deadline() {
    let started = Instant::now();

    let result = sandbox()
        .interpolate(mjml(&tilde_template()), &Variables::new())
        .await;

    let elapsed = started.elapsed();
    eprintln!("tilde: {elapsed:?}");
    assert!(
        matches!(result, Err(InterpolateError::BudgetExceeded)),
        "{result:?}"
    );
    assert!(elapsed < Duration::from_secs(8), "{elapsed:?}");
    wait_for_no_children().await;
}

#[tokio::test]
#[serial]
async fn comparing_two_towers_is_a_permanent_failure_for_every_job_kind() {
    let sandbox = sandbox();
    let source = eq_template();

    let started = Instant::now();
    let mjml_result = sandbox.interpolate(mjml(&source), &Variables::new()).await;
    let plain = Plain::try_new(Some(source.clone()), None).unwrap();
    let plain_result = sandbox
        .interpolate(ResolvedBody::Plain(plain), &Variables::new())
        .await;
    let validated = validate(&sandbox, &source).await;

    let elapsed = started.elapsed();
    eprintln!("eq x3: {elapsed:?}");
    assert!(matches!(mjml_result, Err(InterpolateError::BudgetExceeded)));
    assert!(matches!(
        plain_result,
        Err(InterpolateError::BudgetExceeded)
    ));
    assert!(is_budget(&validated), "{validated:?}");
    assert!(elapsed < Duration::from_secs(12), "{elapsed:?}");
}

#[tokio::test]
#[serial]
async fn a_repeated_list_is_cut_off_by_the_engine_budget() {
    let started = Instant::now();

    let result = validate(&sandbox(), REPEATED_LIST).await;

    eprintln!("repeated list: {:?}", started.elapsed());
    assert!(is_budget(&result), "{result:?}");
}

#[tokio::test]
#[serial]
async fn a_fifty_thousand_deep_document_does_not_take_the_parent_down() {
    let depth = 50_000;
    let doc = format!(
        "<mjml><mj-body>{}x{}</mj-body></mjml>",
        "<mj-wrapper>".repeat(depth),
        "</mj-wrapper>".repeat(depth)
    );
    let sandbox = sandbox();

    let result = sandbox.render(InterpolatedBody::Mjml(doc)).await;

    eprintln!("deep document: {result:?}");
    assert!(matches!(
        result,
        Err(RenderError::Infrastructure { .. } | RenderError::Mjml { .. })
    ));
    assert_eq!(validate(&sandbox, "{{ 1 }}").await.unwrap(), "1");
    wait_for_no_children().await;
}

#[tokio::test]
#[serial]
async fn a_child_that_cannot_allocate_is_an_infrastructure_failure() {
    let sandbox = sandbox_with(RenderSandboxConfig {
        max_memory_bytes: 40_000_000,
        ..config()
    });
    let variables = vars(&[("pad", Value::String("x".repeat(2_500_000)))]);

    let result = sandbox.validate("{{ pad|length }}", &variables).await;

    assert!(is_infra(&result), "{result:?}");
    wait_for_no_children().await;
}

// ---- normal renders ---------------------------------------------------------------------

fn table_rows() -> Variables {
    let rows: Vec<Value> = (0..5000)
        .map(|i| serde_json::json!({ "sku": format!("S{i}"), "name": "widget", "qty": i % 9, "price": 3.5 }))
        .collect();
    vars(&[("rows", Value::Array(rows))])
}

const TABLE: &str = "<mjml><mj-body><mj-section><mj-column><mj-table>{% for r in rows %}<tr><td>{{ r.sku }}</td><td>{{ r.name | upper }}</td><td>{{ r.qty }} x {{ r.price }}</td><td>{% if r.qty > 5 %}bulk{% else %}single{% endif %}</td></tr>{% endfor %}</mj-table></mj-column></mj-section></mj-body></mjml>";

#[tokio::test]
#[serial]
async fn a_big_realistic_table_renders_the_same_in_the_sandbox_as_in_process() {
    let variables = table_rows();
    let sandbox = sandbox();
    let local = MiniJinjaInterpolator::new();
    let renderer = MjmlRenderer::new(IncludeLoaderConfig::default().build().unwrap());

    let remote_body = sandbox.interpolate(mjml(TABLE), &variables).await.unwrap();
    let local_body = local.interpolate(mjml(TABLE), &variables).unwrap();
    let (InterpolatedBody::Mjml(remote_src), InterpolatedBody::Mjml(local_src)) =
        (&remote_body, &local_body)
    else {
        panic!("expected mjml bodies");
    };
    assert_eq!(remote_src, local_src);
    let remote = sandbox.render(remote_body).await.unwrap();
    let local = renderer.render(local_body).await.unwrap();

    assert_eq!(remote.html(), local.html());
    assert_eq!(remote.text(), local.text());
    assert!(remote.html().unwrap().contains("S4999"));
}

#[tokio::test]
#[serial]
async fn the_preview_text_survives_the_round_trip() {
    let sandbox = sandbox();
    let variables = vars(&[("n", Value::String("Tom & Jerry".into()))]);

    let body = sandbox
        .interpolate(mjml(MJML_DOC), &variables)
        .await
        .unwrap();
    let rendered = sandbox.render(body).await.unwrap();

    assert_eq!(rendered.text().unwrap().trim(), "Hi Tom & Jerry");
    assert!(rendered.html().unwrap().contains("Tom &amp; Jerry"));
}

#[tokio::test]
#[serial]
async fn a_plain_body_interpolates_in_a_child_and_renders_without_one() {
    let sandbox = sandbox_with(RenderSandboxConfig {
        program: PathBuf::from("/nonexistent/render-child"),
        ..config()
    });
    let plain = Plain::try_new(Some("hi".into()), Some("<p>hi</p>".into())).unwrap();

    let rendered = sandbox
        .render(InterpolatedBody::Plain(plain.clone()))
        .await
        .unwrap();

    assert_eq!(rendered.text(), Some("hi"));
    assert_eq!(rendered.html(), Some("<p>hi</p>"));
    let spawn_failure = sandbox
        .interpolate(ResolvedBody::Plain(plain), &Variables::new())
        .await;
    assert!(matches!(
        spawn_failure,
        Err(InterpolateError::Infrastructure { .. })
    ));

    let working = self::sandbox();
    let plain = Plain::try_new(Some("hi {{ n }}".into()), Some("<p>{{ n }}</p>".into())).unwrap();
    let variables = vars(&[("n", Value::String("<b>".into()))]);
    let InterpolatedBody::Plain(done) = working
        .interpolate(ResolvedBody::Plain(plain), &variables)
        .await
        .unwrap()
    else {
        panic!("expected a plain body");
    };
    assert_eq!(done.text(), Some("hi <b>"));
    assert_eq!(done.html(), Some("<p>&lt;b&gt;</p>"));
}

#[tokio::test]
#[serial]
async fn an_engine_error_stays_a_typed_failure_not_infrastructure() {
    let sandbox = sandbox();

    let interpolated = sandbox
        .interpolate(mjml("{% if %}"), &Variables::new())
        .await;
    let rendered = sandbox
        .render(InterpolatedBody::Mjml("<mjml><oops".into()))
        .await;

    assert!(matches!(interpolated, Err(InterpolateError::Engine { .. })));
    assert!(matches!(rendered, Err(RenderError::Mjml { .. })));
}

// ---- IPC and protocol -------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn about_three_megabytes_in_and_near_a_megabyte_out_round_trip() {
    let variables = vars(&[
        ("pad", Value::String("x".repeat(2_500_000))),
        ("s", Value::String("y".repeat(900_000))),
    ]);

    let out = sandbox().validate("{{ s }}", &variables).await.unwrap();

    assert_eq!(out.len(), 900_000);
}

#[tokio::test]
#[serial]
async fn a_request_over_four_megabytes_is_an_infrastructure_failure_without_a_child() {
    let variables = vars(&[("pad", Value::String("x".repeat(5 * 1024 * 1024)))]);
    let sandbox = sandbox_with(RenderSandboxConfig {
        program: PathBuf::from("/nonexistent/render-child"),
        ..config()
    });

    let result = sandbox.validate("{{ 1 }}", &variables).await;

    let Err(InterpolateError::Infrastructure { source }) = result else {
        panic!("expected infrastructure, got {result:?}");
    };
    assert!(source.to_string().contains("request"), "{source}");
}

#[tokio::test]
#[serial]
async fn a_response_over_its_cap_is_a_permanent_failure() {
    let sandbox = sandbox_with(RenderSandboxConfig {
        max_response_bytes: 8,
        ..config()
    });

    let result = validate(&sandbox, "{{ 'a long enough answer' }}").await;

    assert!(is_budget(&result), "{result:?}");
    wait_for_no_children().await;
}

#[tokio::test]
#[serial]
async fn a_scripted_child_with_a_complete_response_and_exit_zero_succeeds() {
    let child = script("cat >/dev/null\nprintf '{\"Validated\":\"scripted\"}'");

    let result = validate(&scripted(&child, Duration::from_secs(5)), "x").await;

    assert_eq!(result.unwrap(), "scripted");
}

#[tokio::test]
#[serial]
async fn a_valid_response_with_a_non_zero_exit_is_infrastructure() {
    let child = script("cat >/dev/null\nprintf '{\"Validated\":\"x\"}'\nexit 3");

    let result = validate(&scripted(&child, Duration::from_secs(5)), "x").await;

    assert!(is_infra(&result), "{result:?}");
}

#[tokio::test]
#[serial]
async fn trailing_garbage_truncation_and_silence_are_infrastructure() {
    for body in [
        "cat >/dev/null\nprintf '{\"Validated\":\"x\"}junk'",
        "cat >/dev/null\nprintf '{\"Validated\":'",
        "cat >/dev/null",
    ] {
        let child = script(body);

        let result = validate(&scripted(&child, Duration::from_secs(5)), "x").await;

        assert!(is_infra(&result), "{body}: {result:?}");
    }
}

#[tokio::test]
#[serial]
async fn a_typed_setup_failure_and_an_external_kill_are_infrastructure() {
    let setup = script("cat >/dev/null\nprintf '{\"SetupFailed\":{\"message\":\"no\"}}'\nexit 2");
    let killed = script("kill -9 $$");

    let setup_result = validate(&scripted(&setup, Duration::from_secs(5)), "x").await;
    let killed_result = validate(&scripted(&killed, Duration::from_secs(5)), "x").await;

    assert!(is_infra(&setup_result), "{setup_result:?}");
    assert!(is_infra(&killed_result), "{killed_result:?}");
}

#[tokio::test]
#[serial]
async fn a_child_that_outlives_the_deadline_is_killed_and_the_job_is_permanent() {
    let child = script("cat >/dev/null\nsleep 30");
    let started = Instant::now();

    let result = validate(&scripted(&child, Duration::from_millis(300)), "x").await;

    assert!(is_budget(&result), "{result:?}");
    assert!(started.elapsed() < Duration::from_millis(1300));
    wait_for_no_children().await;
}

#[tokio::test]
#[serial]
async fn a_child_that_answers_and_exits_just_before_the_deadline_is_judged_by_its_result() {
    let child = script("cat >/dev/null\nprintf '{\"Validated\":\"on time\"}'");

    let result = validate(&scripted(&child, Duration::from_millis(1500)), "x").await;

    assert_eq!(result.unwrap(), "on time");
}

#[tokio::test]
#[serial]
async fn the_child_starts_with_an_empty_environment_plus_the_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("env.txt");
    let child = script(&format!(
        "cat >/dev/null\nenv > {}\nprintf '{{\"Validated\":\"x\"}}'",
        out.display()
    ));

    validate(&scripted(&child, Duration::from_secs(5)), "x")
        .await
        .unwrap();

    let env = std::fs::read_to_string(out).unwrap();
    assert!(env.lines().any(|l| l == "MALLOC_ARENA_MAX=2"), "{env}");
    for forbidden in ["PATH=", "HOME=", "CATAPULTE_", "RUST_LOG="] {
        assert!(!env.contains(forbidden), "{forbidden} leaked: {env}");
    }
}

fn run_child_directly(args: &[&str], stdin: &str) -> (Option<i32>, String) {
    use std::io::Write as _;
    let mut child = Command::new(BIN)
        .arg("__render-sandbox")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

#[test]
#[serial]
fn a_child_started_by_the_wrong_parent_or_with_bad_arguments_reports_setup_failure() {
    let job = r#"{"Validate":{"source":"{{ 1 }}","variables":{}}}"#;
    let me = std::process::id().to_string();
    let not_me = (std::process::id() + 1).to_string();

    let cases: [Vec<&str>; 4] = [
        vec!["268435456", "2", "3", &not_me],
        vec!["268435456", "2", "3"],
        vec!["lots", "2", "3", &me],
        vec!["268435456", "-2", "3", &me],
    ];
    for args in cases {
        let (code, stdout) = run_child_directly(&args, job);

        assert_eq!(code, Some(2), "{args:?}: {stdout}");
        assert!(stdout.contains("SetupFailed"), "{args:?}: {stdout}");
    }
    let (code, stdout) = run_child_directly(&["268435456", "2", "3", &me], job);
    assert_eq!((code, stdout.as_str()), (Some(0), r#"{"Validated":"1"}"#));
}

#[test]
#[serial]
fn a_malformed_request_is_a_setup_failure() {
    let me = std::process::id().to_string();

    let (code, stdout) = run_child_directly(&["268435456", "2", "3", &me], "not json");

    assert_eq!(code, Some(2));
    assert!(stdout.contains("SetupFailed"), "{stdout}");
}

// ---- lifecycle --------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn cancelling_a_job_kills_its_child_and_frees_the_slot() {
    let sandbox = sandbox_with(RenderSandboxConfig {
        concurrency: 1,
        ..config()
    });
    let hostile = eq_template();

    let cancelled = tokio::time::timeout(
        Duration::from_millis(100),
        sandbox.validate(&hostile, &Variables::new()),
    )
    .await;

    assert!(
        cancelled.is_err(),
        "the hostile job should still be running"
    );
    wait_for_no_children().await;
    let started = Instant::now();
    assert_eq!(validate(&sandbox, "{{ 1 }}").await.unwrap(), "1");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
#[serial]
async fn a_job_waits_for_a_free_slot_and_gives_up_at_its_deadline() {
    let child = script("cat >/dev/null\nsleep 30");
    let sandbox = sandbox_with(RenderSandboxConfig {
        timeout: Duration::from_millis(600),
        concurrency: 1,
        ..RenderSandboxConfig::new(child.path.clone())
    });

    let (first, second) = tokio::join!(validate(&sandbox, "a"), validate(&sandbox, "b"));

    let results = [first, second];
    assert_eq!(
        results.iter().filter(|r| is_budget(r)).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results.iter().filter(|r| is_infra(r)).count(),
        1,
        "{results:?}"
    );
    wait_for_no_children().await;
}

fn child_set_up(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/oom_score_adj")).is_ok_and(|v| v.trim() == "1000")
}

#[test]
#[serial]
fn a_child_dies_when_the_thread_that_spawned_it_exits() {
    let mut child = std::thread::spawn(|| {
        let child = Command::new(BIN)
            .arg("__render-sandbox")
            .args(["268435456", "2", "3", &std::process::id().to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while !child_set_up(child.id()) {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "child never set up"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        child
    })
    .join()
    .unwrap();

    let status = child.wait().unwrap();

    assert_eq!(status.signal(), Some(9), "{status:?}");
}

#[test]
#[serial]
fn shutting_the_sandbox_down_leaves_no_children() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let sandbox = sandbox();
    let hostile = eq_template();

    runtime.block_on(async {
        let running = sandbox.clone();
        let job = tokio::spawn(async move { running.validate(&hostile, &Variables::new()).await });
        let started = Instant::now();
        while children().is_empty() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "no child started"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        sandbox.shutdown().await;

        assert!(is_infra(&job.await.unwrap()));
        let late = validate(&sandbox, "{{ 1 }}").await;
        assert!(is_infra(&late), "{late:?}");
    });
    assert!(children().is_empty(), "{:?}", children());
    drop(runtime);
    assert!(children().is_empty(), "{:?}", children());
}

#[tokio::test]
#[serial]
async fn an_external_sigkill_of_a_running_child_is_infrastructure() {
    let sandbox = sandbox();
    let hostile = eq_template();
    let running = sandbox.clone();
    let job = tokio::spawn(async move { running.validate(&hostile, &Variables::new()).await });
    let started = Instant::now();
    let pid = loop {
        if let Some(pid) = children().first() {
            break *pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no child started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let killed = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();

    assert!(killed.success());
    let result = job.await.unwrap();
    assert!(is_infra(&result), "{result:?}");
}

// ---- measurements -----------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn spawn_latency_stays_far_below_the_deadline() {
    let sandbox = sandbox();
    let variables = vars(&[("n", Value::String("x".into()))]);
    let mut samples = Vec::new();
    for _ in 0..60 {
        let started = Instant::now();
        sandbox
            .interpolate(mjml("<p>{{ n }}</p>"), &variables)
            .await
            .unwrap();
        samples.push(started.elapsed());
    }

    samples.sort();
    let median = samples[samples.len() / 2];
    let p99 = samples[samples.len() * 99 / 100];
    eprintln!("sandbox latency: median {median:?}, p99 {p99:?}");
    assert!(median < Duration::from_millis(100), "{median:?}");
}

fn peak_vm_kib(job: &str, memory: &str) -> u64 {
    use std::io::Write as _;
    let mut child = Command::new(BIN)
        .arg("__render-sandbox")
        .args([memory, "2", "3", &std::process::id().to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let job = job.to_owned();
    let feeder = std::thread::spawn(move || stdin.write_all(job.as_bytes()).unwrap());
    let drain = std::thread::spawn(move || std::io::copy(&mut stdout, &mut std::io::sink()));
    let mut peak = 0;
    while child.try_wait().unwrap().is_none() {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", child.id()))
            && let Some(kib) = status
                .lines()
                .find_map(|l| l.strip_prefix("VmPeak:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        {
            peak = peak.max(kib);
        }
    }
    feeder.join().unwrap();
    drain.join().unwrap().unwrap();
    peak
}

#[test]
#[serial]
fn the_default_memory_limit_leaves_headroom_over_the_heaviest_normal_job() {
    let table = serde_json::json!({ "Interpolate": { "body": { "Mjml": TABLE }, "variables": table_rows() } });
    let big = serde_json::json!({ "Validate": { "source": "{{ s }}", "variables": {
        "pad": "x".repeat(2_500_000), "s": "y".repeat(900_000) } } });
    let default_limit = 256 * 1024;

    for (name, job) in [("table", table), ("big ipc", big)] {
        let peak = peak_vm_kib(&job.to_string(), "268435456");

        eprintln!("VmPeak {name}: {peak} KiB");
        assert!(peak > 0, "no sample for {name}");
        assert!(peak * 4 <= default_limit, "{name}: {peak} KiB");
    }
}

// ---- startup and shutdown ---------------------------------------------------------------

#[tokio::test]
#[serial]
async fn the_self_test_passes_with_the_real_child_and_fails_without_one() {
    sandbox().self_test().await.unwrap();

    let broken = sandbox_with(RenderSandboxConfig {
        program: PathBuf::from("/nonexistent/render-child"),
        ..config()
    });

    assert!(broken.self_test().await.is_err());
}

fn app_config(http_port: u16, dir: &tempfile::TempDir) -> catapulte::AppConfig {
    use std::collections::HashSet;

    use catapulte::attachment_store::AttachmentStoreBackendConfig;
    use catapulte::publisher::PublisherAdapterConfig;
    use catapulte::queue::QueueBackendConfig;
    use catapulte::storage::StorageBackendConfig;
    use catapulte_inbound_http::InboundHttpConfig;
    use catapulte_inbound_worker::worker::WorkerConfig;
    use catapulte_outbound_attachment_fs::store::FsAttachmentStoreConfig;
    use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
    use catapulte_outbound_smtp::multi_sender::MultiSenderConfig;
    use catapulte_outbound_sqlite::SqliteConfig;

    catapulte::AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", dir.path().join("app.db").display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
            auth: None,
            feedback_secret: None,
            request_timeout: Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: MultiSenderConfig::empty().with_sink_sender("ci", 100),
        resolver: TemplateResolverConfig {
            allowed_domains: HashSet::new(),
            templates_dir: None,
            auth_entries: Vec::new(),
            fetch_timeout: Duration::from_secs(30),
            max_bytes: 1_048_576,
        },
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: dir.path().join("attachments"),
        }),
        attachment_fetcher:
            catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcherConfig::default(),
        include_loader: IncludeLoaderConfig::default(),
        render: config(),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
    }
}

#[tokio::test]
#[serial]
async fn the_shutdown_drain_finishes_in_time_and_leaves_no_children() {
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let app = app_config(port, &dir).build().await.unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let running = tokio::spawn(app.run_with_shutdown(cancel.clone()));
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if reqwest::get(format!("{base}/health/live")).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let hostile = format!(
        "<mjml><mj-body><mj-text>{}</mj-text></mj-body></mjml>",
        eq_template()
    );
    let submitted = reqwest::Client::new()
        .post(format!("{base}/emails"))
        .json(&serde_json::json!({
            "sender": "s@example.com",
            "recipients": [{ "kind": "to", "address": "r@example.com" }],
            "body": { "kind": "mjml_inline", "source": hostile },
            "variables": {}
        }))
        .send()
        .await
        .unwrap();
    assert!(submitted.status().is_success(), "{}", submitted.status());
    let started = Instant::now();
    while children().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no child started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let shutdown_started = Instant::now();
    cancel.cancel();
    running.await.unwrap().unwrap();

    assert!(
        shutdown_started.elapsed() < Duration::from_secs(30),
        "{:?}",
        shutdown_started.elapsed()
    );
    wait_for_no_children().await;
}
