//! Runs template jobs in a fresh, resource-limited child process (the same binary started as
//! `catapulte __render-sandbox`). See the ADR on rendering templates in a subprocess.

use std::borrow::Cow;
use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use catapulte_domain::entity::body::{InterpolatedBody, Plain, RenderedBody, ResolvedBody};
use catapulte_domain::port::template_interpolator::{InterpolateError, TemplateInterpolator};
use catapulte_domain::port::template_renderer::{RenderError, TemplateRenderer};
use catapulte_domain::port::template_validator::TemplateValidator;
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::include_loader::IncludeLoaderConfig;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::Instrument as _;

/// First argument that selects the child mode.
pub const SANDBOX_ARG: &str = "__render-sandbox";

const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const CPU_SOFT_SECS: u64 = 2;
const CPU_HARD_SECS: u64 = 3;
/// Time allowed to reap a child after it was killed. This is cleanup, not render budget.
const REAP_GRACE: Duration = Duration::from_secs(1);
const SETUP_FAILED_EXIT: i32 = 2;

const DEFAULT_TIMEOUT_MS: u64 = 7_000;
const DEFAULT_MAX_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// Variables copied into the child on top of an empty environment. Proxy URLs can hold
/// credentials, so the resulting environment is never logged.
const PASSTHROUGH_ENV: [&str; 13] = [
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "REQUEST_METHOD",
    "LLVM_PROFILE_FILE",
    "TMPDIR",
];

type Variables = Map<String, Value>;

#[derive(Serialize, Deserialize)]
enum WireBody {
    Mjml(String),
    Plain {
        text: Option<String>,
        html: Option<String>,
    },
}

impl WireBody {
    fn from_plain(plain: Plain) -> Self {
        let (text, html) = plain.into_parts();
        Self::Plain { text, html }
    }

    fn into_plain(text: Option<String>, html: Option<String>) -> Result<Plain, anyhow::Error> {
        Plain::try_new(text, html).context("rebuilding plain body")
    }
}

#[derive(Serialize, Deserialize)]
enum Job<'a> {
    Interpolate {
        body: WireBody,
        variables: Cow<'a, Variables>,
    },
    Validate {
        source: Cow<'a, str>,
        variables: Cow<'a, Variables>,
    },
    Render {
        body: WireBody,
        include: Cow<'a, IncludeLoaderConfig>,
    },
}

impl Job<'_> {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Interpolate { .. } => "interpolate",
            Self::Validate { .. } => "validate",
            Self::Render { .. } => "render",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum Outcome {
    Interpolated(WireBody),
    Validated(String),
    Rendered {
        text: Option<String>,
        html: Option<String>,
    },
    /// Fuel or an output cap ran out.
    BudgetExceeded,
    /// The engine rejected the template.
    Failed {
        message: String,
    },
    /// The child could not set itself up.
    SetupFailed {
        message: String,
    },
}

impl std::fmt::Debug for WireBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Mjml(_) => "WireBody::Mjml",
            Self::Plain { .. } => "WireBody::Plain",
        })
    }
}

fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    std::iter::successors(Some(e), |e| e.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

// ---------------------------------------------------------------------------------------------
// Child
// ---------------------------------------------------------------------------------------------

struct ChildArgs {
    memory: u64,
    cpu_soft: u64,
    cpu_hard: u64,
    parent: u32,
}

fn parse_args(args: &[String]) -> anyhow::Result<ChildArgs> {
    let [memory, cpu_soft, cpu_hard, parent] = args else {
        anyhow::bail!("expected 4 arguments, got {}", args.len());
    };
    let number = |name: &str, raw: &String| {
        raw.parse::<u64>()
            .with_context(|| format!("invalid {name} argument"))
    };
    Ok(ChildArgs {
        memory: number("memory", memory)?,
        cpu_soft: number("cpu_soft", cpu_soft)?,
        cpu_hard: number("cpu_hard", cpu_hard)?,
        parent: parent.parse().context("invalid parent pid argument")?,
    })
}

#[cfg(target_os = "linux")]
fn guard_parent(expected: u32) -> anyhow::Result<()> {
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
        .context("setting the parent death signal")?;
    let parent = rustix::process::getppid().map(|pid| pid.as_raw_nonzero().get().cast_unsigned());
    anyhow::ensure!(
        parent == Some(expected),
        "the parent is not the expected process"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn guard_parent(_: u32) -> anyhow::Result<()> {
    Ok(())
}

/// Sets both limits to `soft`/`hard`, never above what the process already has.
fn limit(resource: rustix::process::Resource, soft: u64, hard: u64) -> anyhow::Result<()> {
    let current = rustix::process::getrlimit(resource);
    let clamp = |wanted: u64, ceiling: Option<u64>| ceiling.map_or(wanted, |c| wanted.min(c));
    rustix::process::setrlimit(
        resource,
        rustix::process::Rlimit {
            current: Some(clamp(soft, current.maximum)),
            maximum: Some(clamp(hard, current.maximum)),
        },
    )
    .with_context(|| format!("setting {resource:?} limit"))
}

fn apply_limits(args: &ChildArgs) -> anyhow::Result<()> {
    use rustix::process::Resource;
    limit(Resource::Core, 0, 0)?;
    limit(Resource::Cpu, args.cpu_soft, args.cpu_hard)?;
    let memory = limit(Resource::As, args.memory, args.memory);
    if cfg!(target_os = "linux") {
        memory?;
    }
    // Best effort: prefer this process as the OOM victim.
    #[cfg(target_os = "linux")]
    let _ = std::fs::write("/proc/self/oom_score_adj", "1000");
    Ok(())
}

fn read_request() -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .context("reading the request")?;
    anyhow::ensure!(buf.len() <= MAX_REQUEST_BYTES, "the request is too large");
    Ok(buf)
}

fn from_interpolate_error(e: &InterpolateError) -> Outcome {
    match e {
        InterpolateError::BudgetExceeded => Outcome::BudgetExceeded,
        InterpolateError::Engine { .. } | InterpolateError::Infrastructure { .. } => {
            Outcome::Failed {
                message: error_chain(e),
            }
        }
    }
}

async fn run_job(job: Job<'_>) -> anyhow::Result<Outcome> {
    Ok(match job {
        Job::Interpolate { body, variables } => {
            let body = match body {
                WireBody::Mjml(source) => ResolvedBody::Mjml(source),
                WireBody::Plain { text, html } => {
                    ResolvedBody::Plain(WireBody::into_plain(text, html)?)
                }
            };
            match MiniJinjaInterpolator::new().interpolate(body, &variables) {
                Ok(InterpolatedBody::Mjml(source)) => Outcome::Interpolated(WireBody::Mjml(source)),
                Ok(InterpolatedBody::Plain(plain)) => {
                    Outcome::Interpolated(WireBody::from_plain(plain))
                }
                Err(e) => from_interpolate_error(&e),
            }
        }
        Job::Validate { source, variables } => {
            match MiniJinjaInterpolator::new().validate(&source, &variables) {
                Ok(rendered) => Outcome::Validated(rendered),
                Err(e) => from_interpolate_error(&e),
            }
        }
        Job::Render { body, include } => {
            let body = match body {
                WireBody::Mjml(source) => InterpolatedBody::Mjml(source),
                WireBody::Plain { text, html } => {
                    InterpolatedBody::Plain(WireBody::into_plain(text, html)?)
                }
            };
            let loader = include
                .into_owned()
                .build()
                .context("building the include loader")?;
            match MjmlRenderer::new(loader).render(body).await {
                Ok(rendered) => {
                    let plain = rendered.into_plain();
                    let (text, html) = plain.into_parts();
                    Outcome::Rendered { text, html }
                }
                Err(RenderError::BudgetExceeded) => Outcome::BudgetExceeded,
                Err(e @ (RenderError::Mjml { .. } | RenderError::Infrastructure { .. })) => {
                    Outcome::Failed {
                        message: error_chain(&e),
                    }
                }
            }
        }
    })
}

fn child_main(args: &[String]) -> anyhow::Result<Outcome> {
    let args = parse_args(args)?;
    guard_parent(args.parent)?;
    apply_limits(&args)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .context("building the runtime")?;
    let request = read_request()?;
    let job: Job = serde_json::from_slice(&request).context("decoding the request")?;
    drop(request);
    runtime.block_on(run_job(job))
}

/// Entry point of `catapulte __render-sandbox <memory> <cpu_soft> <cpu_hard> <parent_pid>`.
/// Writes one outcome to stdout and returns the process exit code. Call it before any
/// runtime, telemetry or tracing setup.
#[must_use]
pub fn run_child(args: &[String]) -> i32 {
    let (outcome, code) = match child_main(args) {
        Ok(outcome) => (outcome, 0),
        Err(e) => (
            Outcome::SetupFailed {
                message: format!("{e:#}"),
            },
            SETUP_FAILED_EXIT,
        ),
    };
    let mut stdout = std::io::stdout().lock();
    let written = serde_json::to_writer(&mut stdout, &outcome)
        .map_err(std::io::Error::from)
        .and_then(|()| stdout.flush());
    if written.is_err() {
        return SETUP_FAILED_EXIT;
    }
    code
}

// ---------------------------------------------------------------------------------------------
// Parent
// ---------------------------------------------------------------------------------------------

/// Settings of the render sandbox.
#[derive(Clone, Debug)]
pub struct RenderSandboxConfig {
    /// The binary to start in sandbox mode, normally the current executable.
    pub program: PathBuf,
    /// One absolute deadline per stage: admission, I/O and reaping included.
    pub timeout: Duration,
    /// Address-space limit of each child.
    pub max_memory_bytes: u64,
    /// Children running at the same time.
    pub concurrency: usize,
    /// Test hook: lowers the response cap.
    #[doc(hidden)]
    pub max_response_bytes: usize,
}

fn parse_positive(
    lookup: &impl Fn(&str) -> Result<String, std::env::VarError>,
    key: &str,
    default: u64,
) -> anyhow::Result<u64> {
    match lookup(key) {
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!("{key} is not valid unicode"),
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 => Ok(n),
            _ => anyhow::bail!("{key} must be a positive integer, got {raw:?}"),
        },
    }
}

impl RenderSandboxConfig {
    /// Defaults: 7 s deadline, 256 MiB per child, one child per available core.
    #[must_use]
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            concurrency: default_concurrency(),
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    }

    /// Reads `CATAPULTE_RENDER_TIMEOUT_MS`, `CATAPULTE_RENDER_MAX_MEMORY_BYTES` and
    /// `CATAPULTE_RENDER_CONCURRENCY`, running the current executable as the child.
    ///
    /// # Errors
    ///
    /// Returns an error naming the variable when a value is not unicode, malformed, zero or
    /// too large, or when the current executable cannot be found.
    pub fn from_env() -> anyhow::Result<Self> {
        let program = std::env::current_exe().context("locating the current executable")?;
        Self::from_lookup(program, |key| std::env::var(key))
    }

    fn from_lookup(
        program: PathBuf,
        lookup: impl Fn(&str) -> Result<String, std::env::VarError>,
    ) -> anyhow::Result<Self> {
        let defaults = Self::new(program);
        let concurrency = parse_positive(
            &lookup,
            "CATAPULTE_RENDER_CONCURRENCY",
            defaults.concurrency as u64,
        )?;
        Ok(Self {
            timeout: Duration::from_millis(parse_positive(
                &lookup,
                "CATAPULTE_RENDER_TIMEOUT_MS",
                DEFAULT_TIMEOUT_MS,
            )?),
            max_memory_bytes: parse_positive(
                &lookup,
                "CATAPULTE_RENDER_MAX_MEMORY_BYTES",
                DEFAULT_MAX_MEMORY_BYTES,
            )?,
            concurrency: usize::try_from(concurrency)
                .ok()
                .filter(|n| *n <= Semaphore::MAX_PERMITS)
                .context("CATAPULTE_RENDER_CONCURRENCY is too large")?,
            ..defaults
        })
    }

    /// Checks that two sandbox stages plus repository work fit in both the HTTP request
    /// timeout and the shutdown drain: `2 * (timeout + 1 s) + 5 s <= min(http, drain)`.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeout is zero or the bound is exceeded.
    pub fn check_fits(
        &self,
        http_request_timeout: Duration,
        drain: Duration,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.timeout.is_zero(),
            "CATAPULTE_RENDER_TIMEOUT_MS must be positive"
        );
        let needed = self
            .timeout
            .checked_add(REAP_GRACE)
            .and_then(|stage| stage.checked_mul(2))
            .and_then(|stages| stages.checked_add(Duration::from_secs(5)))
            .context("CATAPULTE_RENDER_TIMEOUT_MS is too large")?;
        let available = http_request_timeout.min(drain);
        anyhow::ensure!(
            needed <= available,
            "2 x (CATAPULTE_RENDER_TIMEOUT_MS + 1 s) + 5 s = {needed:?} must not exceed the \
             smaller of the HTTP request timeout and the shutdown drain ({available:?})"
        );
        Ok(())
    }
}

fn default_concurrency() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// Why a job did not produce a usable outcome.
#[derive(Debug)]
enum SandboxError {
    /// A limit was hit; the label names which one.
    Budget(&'static str),
    Infrastructure(anyhow::Error),
    /// The caller went away or the sandbox is shutting down.
    Cancelled,
}

fn infra(message: impl std::fmt::Display) -> SandboxError {
    SandboxError::Infrastructure(anyhow::anyhow!("{message}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    Code(i32),
    Signal(i32),
    Unknown,
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal().map_or_else(
        || status.code().map_or(Exit::Unknown, Exit::Code),
        Exit::Signal,
    )
}

/// Everything seen about one child, decided after its exit status is known.
#[derive(Default)]
struct Observed {
    /// `None` when the child could not be reaped.
    exit: Option<Exit>,
    /// The parent killed the child at the deadline while it was still running.
    deadline_kill: bool,
    /// The response went over its cap.
    overflow: bool,
    /// The complete stdout, when it was read to EOF.
    response: Option<Vec<u8>>,
    /// An I/O error on the pipes, for the diagnostic.
    io_error: Option<std::io::Error>,
}

/// Decides the final result. Precedence: CPU limit, deadline kill, response overflow, then a
/// complete valid outcome with exit 0; anything else is an infrastructure failure.
fn classify(observed: Observed) -> Result<Outcome, SandboxError> {
    if observed.exit == Some(Exit::Signal(rustix::process::Signal::XCPU.as_raw())) {
        return Err(SandboxError::Budget("cpu"));
    }
    if observed.deadline_kill {
        return Err(SandboxError::Budget("deadline"));
    }
    if observed.overflow {
        return Err(SandboxError::Budget("response_cap"));
    }
    match (observed.exit, observed.response) {
        (Some(Exit::Code(0)), Some(response)) => match parse_response(&response) {
            Ok(Outcome::BudgetExceeded) => Err(SandboxError::Budget("engine")),
            Ok(Outcome::SetupFailed { message }) => Err(infra(format!("setup failed: {message}"))),
            Ok(outcome) => Ok(outcome),
            Err(e) => Err(SandboxError::Infrastructure(e)),
        },
        (exit, _) => {
            let detail = observed
                .io_error
                .map(|e| format!(" ({e})"))
                .unwrap_or_default();
            Err(infra(format!(
                "render child ended without a valid result: exit {exit:?}{detail}"
            )))
        }
    }
}

/// Exactly one complete JSON outcome and nothing after it.
fn parse_response(bytes: &[u8]) -> anyhow::Result<Outcome> {
    let mut stream = serde_json::Deserializer::from_slice(bytes).into_iter::<Outcome>();
    let outcome = stream
        .next()
        .context("the child wrote no response")?
        .context("the child wrote a malformed response")?;
    anyhow::ensure!(
        stream.byte_offset() == bytes.len(),
        "the child wrote data after its response"
    );
    Ok(outcome)
}

/// An `io::Write` that fails once more than `MAX_REQUEST_BYTES` were written.
struct CappedVec(Vec<u8>);

impl std::io::Write for CappedVec {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + data.len() > MAX_REQUEST_BYTES {
            return Err(std::io::Error::other("render request exceeds its cap"));
        }
        self.0.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

enum IoFailure {
    Overflow,
    Io(std::io::Error),
}

/// Writes the request, closes stdin, and reads at most `cap` bytes of response. The first
/// failure of either side ends the exchange.
async fn exchange(
    stdin: tokio::process::ChildStdin,
    mut stdout: tokio::process::ChildStdout,
    request: &[u8],
    cap: usize,
) -> Result<Vec<u8>, IoFailure> {
    let write = async move {
        let mut stdin = stdin;
        stdin.write_all(request).await.map_err(IoFailure::Io)?;
        stdin.shutdown().await.map_err(IoFailure::Io)?;
        drop(stdin);
        Ok(())
    };
    let read = async {
        let mut buf = Vec::new();
        (&mut stdout)
            .take(cap as u64 + 1)
            .read_to_end(&mut buf)
            .await
            .map_err(IoFailure::Io)?;
        if buf.len() > cap {
            Err(IoFailure::Overflow)
        } else {
            Ok(buf)
        }
    };
    tokio::try_join!(write, read).map(|((), response)| response)
}

fn child_env(lookup: impl Fn(&str) -> Option<OsString>) -> Vec<(&'static str, OsString)> {
    let mut env = vec![("MALLOC_ARENA_MAX", OsString::from("2"))];
    env.extend(
        PASSTHROUGH_ENV
            .iter()
            .filter_map(|name| lookup(name).map(|value| (*name, value))),
    );
    env
}

enum Step {
    Cancelled,
    Io(Result<Vec<u8>, IoFailure>),
    Deadline,
}

/// Renders templates in child processes. Cheap to clone; clones share one admission limit.
///
/// The child's parent-death signal follows the OS thread that spawned it. If that runtime
/// worker thread retires, which in practice only happens at runtime shutdown, the child gets
/// SIGKILL and the job fails as an infrastructure error.
#[derive(Clone)]
pub struct RenderSandbox {
    config: Arc<RenderSandboxConfig>,
    include: Arc<IncludeLoaderConfig>,
    permits: Arc<Semaphore>,
    tracker: TaskTracker,
    shutdown: CancellationToken,
}

impl RenderSandbox {
    /// `include` is the effective include policy, taken from the application configuration.
    #[must_use]
    pub fn new(config: RenderSandboxConfig, include: IncludeLoaderConfig) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(config.concurrency)),
            config: Arc::new(config),
            include: Arc::new(include),
            tracker: TaskTracker::new(),
            shutdown: CancellationToken::new(),
        }
    }

    /// Runs one trivial job of each kind, so a missing or broken child binary stops startup.
    ///
    /// # Errors
    ///
    /// Returns an error when the child cannot validate and render a trivial template.
    pub async fn self_test(&self) -> anyhow::Result<()> {
        let validated = self
            .validate("{{ 1 }}", &Variables::new())
            .await
            .context("validating a trivial template")?;
        anyhow::ensure!(
            validated == "1",
            "the render child answered {validated:?} instead of \"1\""
        );
        self.render(InterpolatedBody::Mjml(
            "<mjml><mj-body></mj-body></mjml>".to_owned(),
        ))
        .await
        .context("rendering an empty document")?;
        Ok(())
    }

    /// Kills every running child, refuses new jobs, and waits until all are reaped.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tracker.close();
        self.tracker.wait().await;
    }

    async fn run(&self, job: Job<'_>) -> Result<Outcome, SandboxError> {
        let deadline = Instant::now() + self.config.timeout;
        let mut request = CappedVec(Vec::new());
        serde_json::to_writer(&mut request, &job)
            .map_err(|e| infra(format!("serializing the render request: {e}")))?;

        let span = tracing::info_span!(
            "template.sandbox",
            job = job.kind(),
            outcome = tracing::field::Empty,
            limit = tracing::field::Empty,
        );
        let cancel = self.shutdown.child_token();
        let guard = cancel.clone().drop_guard();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let this = self.clone();
        let owner_span = span.clone();
        self.tracker.spawn(
            async move {
                let result = this.drive(request.0, deadline, &cancel).await;
                match &result {
                    Ok(_) => owner_span.record("outcome", "ok"),
                    Err(SandboxError::Budget(limit)) => {
                        tracing::warn!(limit, "template render hit a limit");
                        owner_span
                            .record("outcome", "budget")
                            .record("limit", *limit)
                    }
                    Err(SandboxError::Infrastructure(e)) => {
                        tracing::warn!(error = %e, "template render sandbox failed");
                        owner_span.record("outcome", "infrastructure")
                    }
                    Err(SandboxError::Cancelled) => owner_span.record("outcome", "cancelled"),
                };
                let _ = tx.send(result);
            }
            .instrument(span),
        );
        let result = rx
            .await
            .unwrap_or_else(|_| Err(infra("the render task ended without a result")));
        guard.disarm();
        result
    }

    async fn drive(
        &self,
        request: Vec<u8>,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Outcome, SandboxError> {
        let _permit = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(SandboxError::Cancelled),
            acquired = tokio::time::timeout_at(deadline, self.permits.acquire()) => match acquired {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => return Err(infra("the render admission queue is closed")),
                Err(_) => return Err(infra("timed out waiting for a render slot")),
            },
        };
        if cancel.is_cancelled() {
            return Err(SandboxError::Cancelled);
        }

        let mut child = self.spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(infra("the render child has no pipes"));
        };
        let mut io = std::pin::pin!(exchange(
            stdin,
            stdout,
            &request,
            self.config.max_response_bytes
        ));

        let step = tokio::select! {
            biased;
            () = cancel.cancelled() => Step::Cancelled,
            result = &mut io => Step::Io(result),
            () = tokio::time::sleep_until(deadline) => Step::Deadline,
        };

        let mut observed = Observed::default();
        // None: the child is still running and must be killed before it is reaped.
        let mut natural_exit = None;
        match step {
            Step::Cancelled => {
                kill_and_reap(&mut child).await;
                return Err(SandboxError::Cancelled);
            }
            Step::Io(Ok(response)) => {
                observed.response = Some(response);
                match tokio::time::timeout_at(deadline, child.wait()).await {
                    Ok(Ok(status)) => natural_exit = Some(exit_of(status)),
                    Ok(Err(e)) => observed.io_error = Some(e),
                    Err(_) => observed.deadline_kill = true,
                }
            }
            Step::Io(Err(IoFailure::Overflow)) => observed.overflow = true,
            Step::Io(Err(IoFailure::Io(e))) => observed.io_error = Some(e),
            Step::Deadline => match child.try_wait() {
                // The child finished first: judge it by its own result.
                Ok(Some(status)) => {
                    natural_exit = Some(exit_of(status));
                    match tokio::time::timeout(REAP_GRACE, &mut io).await {
                        Ok(Ok(response)) => observed.response = Some(response),
                        Ok(Err(IoFailure::Overflow)) => observed.overflow = true,
                        Ok(Err(IoFailure::Io(e))) => observed.io_error = Some(e),
                        Err(_) => {}
                    }
                }
                _ => observed.deadline_kill = true,
            },
        }
        observed.exit = match natural_exit {
            Some(exit) => Some(exit),
            None => kill_and_reap(&mut child).await,
        };
        classify(observed)
    }

    fn spawn(&self) -> Result<tokio::process::Child, SandboxError> {
        let mut command = tokio::process::Command::new(&self.config.program);
        command
            .arg(SANDBOX_ARG)
            .arg(self.config.max_memory_bytes.to_string())
            .arg(CPU_SOFT_SECS.to_string())
            .arg(CPU_HARD_SECS.to_string())
            .arg(std::process::id().to_string())
            .env_clear()
            .envs(child_env(|name| std::env::var_os(name)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        command
            .spawn()
            .map_err(|e| infra(format!("spawning the render child: {e}")))
    }
}

/// Kills the child if it still runs, then waits for it so it is never left as a zombie.
async fn kill_and_reap(child: &mut tokio::process::Child) -> Option<Exit> {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.start_kill();
    }
    match tokio::time::timeout(REAP_GRACE, child.wait()).await {
        Ok(Ok(status)) => Some(exit_of(status)),
        _ => None,
    }
}

fn interpolate_error(e: SandboxError) -> InterpolateError {
    match e {
        SandboxError::Budget(_) => InterpolateError::BudgetExceeded,
        SandboxError::Infrastructure(source) => InterpolateError::Infrastructure { source },
        SandboxError::Cancelled => InterpolateError::Infrastructure {
            source: anyhow::anyhow!("the render was cancelled"),
        },
    }
}

fn render_error(e: SandboxError) -> RenderError {
    match e {
        SandboxError::Budget(_) => RenderError::BudgetExceeded,
        SandboxError::Infrastructure(source) => RenderError::Infrastructure { source },
        SandboxError::Cancelled => RenderError::Infrastructure {
            source: anyhow::anyhow!("the render was cancelled"),
        },
    }
}

fn unexpected(outcome: &Outcome) -> anyhow::Error {
    anyhow::anyhow!("the render child answered a different job: {outcome:?}")
}

impl TemplateInterpolator for RenderSandbox {
    async fn interpolate(
        &self,
        body: ResolvedBody,
        variables: &Variables,
    ) -> Result<InterpolatedBody, InterpolateError> {
        let body = match body {
            ResolvedBody::Mjml(source) => WireBody::Mjml(source),
            ResolvedBody::Plain(plain) => WireBody::from_plain(plain),
        };
        let job = Job::Interpolate {
            body,
            variables: Cow::Borrowed(variables),
        };
        match self.run(job).await.map_err(interpolate_error)? {
            Outcome::Interpolated(WireBody::Mjml(source)) => Ok(InterpolatedBody::Mjml(source)),
            Outcome::Interpolated(WireBody::Plain { text, html }) => {
                WireBody::into_plain(text, html)
                    .map(InterpolatedBody::Plain)
                    .map_err(|source| InterpolateError::Infrastructure { source })
            }
            Outcome::Failed { message } => Err(InterpolateError::Engine {
                source: anyhow::anyhow!(message),
            }),
            other => Err(InterpolateError::Infrastructure {
                source: unexpected(&other),
            }),
        }
    }
}

impl TemplateValidator for RenderSandbox {
    async fn validate(
        &self,
        source: &str,
        variables: &Variables,
    ) -> Result<String, InterpolateError> {
        let job = Job::Validate {
            source: Cow::Borrowed(source),
            variables: Cow::Borrowed(variables),
        };
        match self.run(job).await.map_err(interpolate_error)? {
            Outcome::Validated(rendered) => Ok(rendered),
            Outcome::Failed { message } => Err(InterpolateError::Engine {
                source: anyhow::anyhow!(message),
            }),
            other => Err(InterpolateError::Infrastructure {
                source: unexpected(&other),
            }),
        }
    }
}

impl TemplateRenderer for RenderSandbox {
    async fn render(&self, body: InterpolatedBody) -> Result<RenderedBody, RenderError> {
        let source = match body {
            // A plain body needs no rendering, so it never costs a child.
            InterpolatedBody::Plain(plain) => return Ok(RenderedBody::new(plain)),
            InterpolatedBody::Mjml(source) => source,
        };
        let job = Job::Render {
            body: WireBody::Mjml(source),
            include: Cow::Borrowed(self.include.as_ref()),
        };
        match self.run(job).await.map_err(render_error)? {
            Outcome::Rendered { text, html } => WireBody::into_plain(text, html)
                .map(RenderedBody::new)
                .map_err(|source| RenderError::Infrastructure { source }),
            Outcome::Failed { message } => Err(RenderError::Mjml {
                source: anyhow::anyhow!(message),
            }),
            other => Err(RenderError::Infrastructure {
                source: unexpected(&other),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::env::VarError;

    use super::*;

    const XCPU: i32 = 24;
    const VALID: &[u8] = br#"{"Validated":"ok"}"#;

    fn done(exit: Exit, response: &[u8]) -> Observed {
        Observed {
            exit: Some(exit),
            response: Some(response.to_vec()),
            ..Observed::default()
        }
    }

    fn is_budget(result: &Result<Outcome, SandboxError>) -> bool {
        matches!(result, Err(SandboxError::Budget(_)))
    }

    fn is_infra(result: &Result<Outcome, SandboxError>) -> bool {
        matches!(result, Err(SandboxError::Infrastructure(_)))
    }

    #[test]
    fn a_complete_valid_response_with_exit_zero_is_the_outcome() {
        let result = classify(done(Exit::Code(0), VALID));

        assert!(matches!(result, Ok(Outcome::Validated(v)) if v == "ok"));
    }

    #[test]
    fn a_typed_failure_is_an_outcome_and_a_typed_budget_is_permanent() {
        let failed = classify(done(Exit::Code(0), br#"{"Failed":{"message":"bad"}}"#));
        let budget = classify(done(Exit::Code(0), br#""BudgetExceeded""#));

        assert!(matches!(failed, Ok(Outcome::Failed { message }) if message == "bad"));
        assert!(is_budget(&budget));
    }

    #[test]
    fn a_valid_response_with_a_non_zero_exit_is_infrastructure() {
        assert!(is_infra(&classify(done(Exit::Code(3), VALID))));
    }

    #[test]
    fn a_typed_setup_failure_is_infrastructure() {
        let result = classify(done(
            Exit::Code(0),
            br#"{"SetupFailed":{"message":"no limits"}}"#,
        ));

        assert!(is_infra(&result));
    }

    #[test]
    fn trailing_garbage_or_a_truncated_or_missing_response_is_infrastructure() {
        for response in [
            &b"{\"Validated\":\"ok\"}x"[..],
            b"{\"Validated\":\"ok\"} ",
            b"{\"Valid",
            b"",
        ] {
            assert!(
                is_infra(&classify(done(Exit::Code(0), response))),
                "{response:?}"
            );
        }
        let two = br#"{"Validated":"a"}{"Validated":"b"}"#;
        assert!(is_infra(&classify(done(Exit::Code(0), two))));
        assert!(is_infra(&classify(Observed {
            exit: Some(Exit::Code(0)),
            ..Observed::default()
        })));
    }

    #[test]
    fn the_cpu_signal_is_budget_even_after_an_io_error() {
        let result = classify(Observed {
            exit: Some(Exit::Signal(XCPU)),
            io_error: Some(std::io::Error::other("broken pipe")),
            ..Observed::default()
        });

        assert!(is_budget(&result));
    }

    #[test]
    fn any_other_signal_or_an_unreaped_child_is_infrastructure() {
        for exit in [
            Exit::Signal(6),
            Exit::Signal(9),
            Exit::Signal(11),
            Exit::Unknown,
        ] {
            assert!(is_infra(&classify(done(exit, VALID))), "{exit:?}");
        }
        assert!(is_infra(&classify(Observed::default())));
    }

    #[test]
    fn a_deadline_kill_and_a_response_overflow_are_budget_but_a_finished_child_is_not() {
        let killed = Observed {
            exit: Some(Exit::Signal(9)),
            deadline_kill: true,
            ..Observed::default()
        };
        let overflow = Observed {
            exit: Some(Exit::Signal(9)),
            overflow: true,
            ..Observed::default()
        };
        // The deadline fired but the child had already finished: no deadline kill was decided.
        let finished = done(Exit::Code(0), VALID);

        assert!(is_budget(&classify(killed)));
        assert!(is_budget(&classify(overflow)));
        assert!(matches!(classify(finished), Ok(Outcome::Validated(_))));
    }

    #[test]
    fn the_cpu_signal_wins_over_a_decided_deadline_kill_and_a_valid_response() {
        let mut observed = done(Exit::Signal(XCPU), VALID);
        observed.deadline_kill = true;
        observed.overflow = true;

        assert!(matches!(
            classify(observed),
            Err(SandboxError::Budget("cpu"))
        ));
    }

    #[test]
    fn the_child_environment_is_an_allowlist() {
        let vars: HashMap<&str, &str> = HashMap::from([
            ("HTTPS_PROXY", "http://user:pw@proxy:3128"),
            ("no_proxy", "localhost"),
            ("LLVM_PROFILE_FILE", "cov-%p-%m.profraw"),
            ("PATH", "/usr/bin"),
            ("CATAPULTE_BOUNCE_SECRET", "secret"),
            ("RUST_LOG", "debug"),
        ]);

        let env = child_env(|name| vars.get(name).map(OsString::from));
        let names: Vec<&str> = env.iter().map(|(name, _)| *name).collect();

        assert_eq!(
            names,
            [
                "MALLOC_ARENA_MAX",
                "HTTPS_PROXY",
                "no_proxy",
                "LLVM_PROFILE_FILE"
            ]
        );
        assert_eq!(env[0].1, "2");
        assert_eq!(env[3].1, "cov-%p-%m.profraw");
    }

    #[test]
    fn without_coverage_variables_nothing_extra_is_passed() {
        let env = child_env(|_| None);

        assert_eq!(env.len(), 1);
    }

    fn lookup(
        vars: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Result<String, VarError> {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
                .ok_or(VarError::NotPresent)
        }
    }

    #[test]
    fn the_config_defaults_apply_when_nothing_is_set() {
        let config = RenderSandboxConfig::from_lookup("p".into(), lookup(&[])).unwrap();

        assert_eq!(config.timeout, Duration::from_secs(7));
        assert_eq!(config.max_memory_bytes, 268_435_456);
        assert_eq!(config.concurrency, default_concurrency());
    }

    #[test]
    fn the_config_reads_each_variable() {
        let config = RenderSandboxConfig::from_lookup(
            "p".into(),
            lookup(&[
                ("CATAPULTE_RENDER_TIMEOUT_MS", "1500"),
                ("CATAPULTE_RENDER_MAX_MEMORY_BYTES", "1048576"),
                ("CATAPULTE_RENDER_CONCURRENCY", "3"),
            ]),
        )
        .unwrap();

        assert_eq!(config.timeout, Duration::from_millis(1500));
        assert_eq!(config.max_memory_bytes, 1_048_576);
        assert_eq!(config.concurrency, 3);
    }

    #[test]
    fn bad_config_values_name_their_key() {
        for key in [
            "CATAPULTE_RENDER_TIMEOUT_MS",
            "CATAPULTE_RENDER_MAX_MEMORY_BYTES",
            "CATAPULTE_RENDER_CONCURRENCY",
        ] {
            for bad in ["0", "-1", "abc", "", "99999999999999999999999"] {
                let vars: &'static [(&str, &str)] = Box::leak(Box::new([(
                    Box::leak(key.to_owned().into_boxed_str()) as &str,
                    bad,
                )]));
                let err = RenderSandboxConfig::from_lookup("p".into(), lookup(vars)).unwrap_err();
                assert!(err.to_string().contains(key), "{key}={bad:?}: {err}");
            }
            let not_unicode = |_: &str| Err(VarError::NotUnicode(OsString::from("x")));
            let err = RenderSandboxConfig::from_lookup("p".into(), not_unicode).unwrap_err();
            assert!(err.to_string().contains("CATAPULTE_RENDER"), "{err}");
        }
    }

    #[test]
    fn the_stage_bound_must_fit_the_http_timeout_and_the_drain() {
        let config = |ms| RenderSandboxConfig {
            timeout: Duration::from_millis(ms),
            ..RenderSandboxConfig::new("p".into())
        };
        let thirty = Duration::from_secs(30);

        assert!(config(7000).check_fits(thirty, thirty).is_ok());
        assert!(config(11_500).check_fits(thirty, thirty).is_ok());
        assert!(config(11_501).check_fits(thirty, thirty).is_err());
        assert!(
            config(7000)
                .check_fits(Duration::from_secs(20), thirty)
                .is_err()
        );
        assert!(
            config(7000)
                .check_fits(thirty, Duration::from_secs(20))
                .is_err()
        );
        assert!(config(0).check_fits(thirty, thirty).is_err());
        let huge = RenderSandboxConfig {
            timeout: Duration::MAX,
            ..RenderSandboxConfig::new("p".into())
        };
        assert!(huge.check_fits(thirty, thirty).is_err());
    }

    #[test]
    fn child_arguments_are_parsed_strictly() {
        let args = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert!(parse_args(&args(&["1", "2", "3", "4"])).is_ok());
        for bad in [
            args(&["1", "2", "3"]),
            args(&["1", "2", "3", "4", "5"]),
            args(&["x", "2", "3", "4"]),
            args(&["1", "-2", "3", "4"]),
            args(&["1", "2", "3", ""]),
        ] {
            assert!(parse_args(&bad).is_err(), "{bad:?}");
        }
    }
}
