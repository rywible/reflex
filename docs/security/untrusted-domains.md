# Untrusted domain and verifier boundary

External candidate generators and verifiers are untrusted. Reflex launches an
owned external process only through `DomainWorkerSupervisor::launch_sandboxed`.
There is no public API for attaching an arbitrary child process.

On Linux the boundary requires both `bwrap` and `prlimit`; absence is a durable
`IsolationUnavailable` failure, never a request to run without isolation. The
launcher:

- verifies the pinned BLAKE3 digest of the executable before spawning it;
- maps a workspace-relative executable beneath read-only `/work`, or an exact
  `/usr`/`/bin` executable at its read-only canonical system path;
- mounts the immutable input root read-only and a dedicated scratch directory
  outside that tree at `/scratch` and `/tmp` as the only writable filesystem;
- creates user, PID, IPC, UTS, and (by default) network namespaces, runs as the
  namespace's uid/gid 65534, starts a new session, and installs address-space,
  process, and file-descriptor limits;
- clears the environment and admits only `LANG`, `LC_ALL`, and `TZ`; child
  `HOME` and `TMPDIR` point to scratch;
- bounds aggregate arguments/environment by the protocol frame limit.

The trusted host retains verifier authority, accepted-attempt fencing, CAS
publication, and metadata credentials. A child cannot publish accepted
metadata. Output containing credential-shaped material is rejected, and log
redaction handles repeated, mixed-case, quoted, and variably spaced Bearer
credentials.

Lean kernel execution follows the same rule. `LeanDomain::new()` has no launch
authority and fails closed; callers must provide an explicit
`KernelSandboxConfig` containing the immutable root, dedicated scratch path,
exact executable path, and pinned executable digest.

Remaining assumptions are the Linux kernel, bubblewrap, prlimit, the read-only
system libraries mapped into the namespace, and the trusted Reflex host
process. macOS has no claimed strong backend in v1 and therefore fails closed.
