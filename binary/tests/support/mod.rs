use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use testcontainers::core::ContainerRequest;
use testcontainers::{GenericImage, ImageExt};

const LABEL_KEY: &str = "catapulte.test.run";

// Identifies this test process; a detached watcher removes every container carrying it.
static RUN: LazyLock<String> = LazyLock::new(|| {
    let docker = Command::new("docker")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to run `docker`; install Docker and make sure it is on PATH");
    assert!(
        docker.success(),
        "Docker is not reachable; install and start Docker to run these tests"
    );
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_nanos();
    let value = format!("{pid}-{nanos}");
    // Own process group so the watcher outlives a killed test binary and its group.
    // The child is never reaped on purpose: it must outlive this process.
    #[allow(clippy::zombie_processes)]
    let _watcher = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "while kill -0 {pid} 2>/dev/null; do sleep 1; done\n\
             sleep 2\n\
             empty=0\n\
             for _ in $(seq 60); do\n\
             if ids=$(docker ps -aq --filter label={LABEL_KEY}={value}); then\n\
             if [ -z \"$ids\" ]; then\n\
             empty=$((empty + 1))\n\
             [ $empty -ge 2 ] && exit 0\n\
             else\n\
             empty=0\n\
             docker rm -f -v $ids >/dev/null 2>&1\n\
             fi\n\
             else\n\
             empty=0\n\
             fi\n\
             sleep 1\n\
             done"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("failed to spawn the test container watcher");
    value
});

pub fn labelled(request: impl ImageExt<GenericImage>) -> ContainerRequest<GenericImage> {
    LazyLock::force(&RUN);
    request.with_label(LABEL_KEY, RUN.as_str())
}
