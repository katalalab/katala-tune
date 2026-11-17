//! One directly awaited child, finite writer and bounded drainers. No System runner.
use super::{
    RemoteUpdateFuture, RemoteUpdatePayload, RemoteUpdateReply, RemoteUpdateStage, RemoteUpdateTransport,
    protocol::{self, Image, Parser, SamplerHash},
};
use crate::{collect, nodes::Node};
use std::{io, process::Stdio};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, Command},
};

#[derive(Clone, Copy)]
enum NativeBackend {
    Mac,
    Windows,
}

type CommandFactory = dyn Fn(&Node) -> io::Result<Command> + Send + Sync;
/// Opt-in native transport. Only a bound native observation can place bytes.
pub struct OwnedSshUpdateTransport {
    command: Box<CommandFactory>,
    context: SamplerHash,
    glue: &'static [u8],
    native: Option<NativeBackend>,
}
impl RemoteUpdateTransport for OwnedSshUpdateTransport {
    fn preflight(&self, node: &Node, payload: RemoteUpdatePayload<'_>) -> Result<(), super::Refusal> {
        match self.native {
            Some(NativeBackend::Mac) => super::macos::preflight(node, payload),
            Some(NativeBackend::Windows) => super::windows::preflight(node, payload),
            None => Ok(()),
        }
    }
    fn place<'a>(&'a self, node: &'a Node, payload: RemoteUpdatePayload<'a>) -> RemoteUpdateFuture<'a> {
        Box::pin(async move {
            if let Err(reason) = self.preflight(node, payload.request.payload()) {
                let mut reply = unknown();
                reply.before_start_refusal = Some(reason);
                return reply;
            }
            let context = if self.native.is_some() { super::observation::place_context(payload.destination_observation().unwrap()) } else { self.context };
            let mut image = Image::payload(&payload, context, self.glue);
            if self.native.is_some() {
                image.ttl_ms = match super::observation::remaining(payload.request) {
                    Ok(ttl) => ttl,
                    Err(reason) => {
                        let mut reply = unknown();
                        reply.before_start_refusal = Some(reason);
                        return reply;
                    }
                };
                image.observation = payload.destination_observation().unwrap().snapshot.record();
            } else {
                // Trusted synthetic executor fixtures keep a fixed before image.
                #[cfg(test)]
                {
                    image.before = super::BeforeImageFact::Present(SamplerHash::parse(&"b".repeat(64)).unwrap());
                }
            }
            self.execute_image(node, &image).await.reply
        })
    }
}
pub(super) struct ExchangeResult {
    pub reply: RemoteUpdateReply,
    pub observed: Option<super::observation::DestinationSnapshot>,
    pub failure: Option<super::macos::InspectFailure>,
}
pub(super) fn native_transport() -> OwnedSshUpdateTransport {
    OwnedSshUpdateTransport {
        command: Box::new(|node| ssh_command(node, &super::macos::launcher())),
        context: SamplerHash::parse(&"0".repeat(64)).unwrap(),
        glue: super::macos::GLUE,
        native: Some(NativeBackend::Mac),
    }
}
pub(super) fn windows_native() -> OwnedSshUpdateTransport {
    OwnedSshUpdateTransport {
        command: Box::new(|node| ssh_command(node, &super::windows::launcher())),
        context: SamplerHash::parse(&"0".repeat(64)).unwrap(),
        glue: super::windows::GLUE,
        native: Some(NativeBackend::Windows),
    }
}
impl OwnedSshUpdateTransport {
    pub(super) fn is_native(&self) -> bool {
        self.native.is_some()
    }
    pub(super) fn is_windows(&self) -> bool {
        matches!(self.native, Some(NativeBackend::Windows))
    }
    pub(super) fn native_glue(&self) -> &'static [u8] {
        self.glue
    }
    pub(super) async fn execute_image(&self, node: &Node, image: &Image<'_>) -> ExchangeResult {
        let empty = || ExchangeResult { reply: unknown(), observed: None, failure: None };
        let Some((prefix, header)) = image.header().filter(|_| valid_alias(&node.alias)) else {
            return empty();
        };
        let Ok(mut command) = (self.command)(node) else {
            return empty();
        };
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(false);
        let Ok(mut child) = command.spawn() else {
            return empty();
        };
        exchange(&mut child, image, &prefix, &header, || {}).await
    }
}
fn unknown() -> RemoteUpdateReply {
    RemoteUpdateReply::observed(None, b"", RemoteUpdateStage::Started, false)
}
fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 255
        && alias.as_bytes()[0].is_ascii_alphanumeric()
        && alias.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
/// Native renderer calls this private builder with its fixed launcher.
/// Never invoke the legacy timeout/kill runner. Options are built from scratch:
/// OpenSSH uses the first value, so appending over SSH_OPTS is not an override.
fn ssh_command(node: &Node, fixed_launcher: &str) -> io::Result<Command> {
    if !valid_alias(&node.alias) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut command = collect::command("ssh");
    for option in [
        "BatchMode=yes",
        "ConnectionAttempts=1",
        "ConnectTimeout=10",
        "ControlMaster=no",
        "ControlPath=none",
        "StrictHostKeyChecking=yes",
        "ServerAliveInterval=0",
    ] {
        command.args(["-o", option]);
    }
    let remote = collect::remote_command(node, fixed_launcher);
    // ASCII wire bound includes the existing Git Bash outer wrapper. The
    // doubled bound also covers Windows argument quoting/backslash expansion.
    if node.is_windows() && (!remote.is_ascii() || remote.len() > 7900) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    command.arg(&node.alias).arg(remote);
    if node.is_windows() && command_line_bound(&command).is_none_or(|units| units > 32767) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    Ok(command)
}
fn utf16_units(value: &std::ffi::OsStr) -> usize {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        value.encode_wide().count()
    }
    #[cfg(not(windows))]
    {
        value.to_string_lossy().encode_utf16().count()
    }
}
fn command_line_bound(command: &Command) -> Option<usize> {
    let initial = utf16_units(command.as_std().get_program()).checked_mul(2)?.checked_add(3)?;
    command.as_std().get_args().try_fold(initial, |units, arg| units.checked_add(utf16_units(arg).checked_mul(2)?.checked_add(3)?))
}
pub(super) async fn write_image(mut stdin: impl AsyncWrite + Unpin, image: &Image<'_>, prefix: &str, header: &str) -> io::Result<()> {
    for part in [prefix.as_bytes(), header.as_bytes(), image.glue, image.bytes] {
        for chunk in part.chunks(protocol::CHUNK) {
            stdin.write_all(chunk).await?;
        }
    }
    // Normal EOF is part of the protocol; not an early cancellation signal.
    stdin.shutdown().await
}
async fn stdout(mut pipe: impl AsyncRead + Unpin, parser: &mut Parser) -> bool {
    let mut scratch = [0; 4096];
    loop {
        match pipe.read(&mut scratch).await {
            Ok(0) => return true,
            Ok(n) => parser.feed(&scratch[..n]),
            Err(_) => return false,
        }
    }
}
async fn stderr(mut pipe: impl AsyncRead + Unpin) -> bool {
    let mut scratch = [0; 4096];
    let mut observed = 0usize;
    loop {
        match pipe.read(&mut scratch).await {
            Ok(0) => return observed <= protocol::STDERR_LIMIT,
            Ok(n) => observed = observed.saturating_add(n).min(protocol::STDERR_LIMIT + 1),
            Err(_) => return false,
        }
    }
}
/// Child remains owned by the directly awaiting caller. The private on_wait
/// observer lets tests prove *actual* root wait while descendant EOF is held;
/// it does not replace wait or any production executor/parser/writer branch.
async fn exchange(child: &mut Child, image: &Image<'_>, prefix: &str, header: &str, on_wait: impl FnOnce()) -> ExchangeResult {
    let stdin = child.stdin.take().expect("owned piped stdin");
    let out = child.stdout.take().expect("owned piped stdout");
    let err = child.stderr.take().expect("owned piped stderr");
    let mut parser = Parser::new(image);
    let wait = async {
        match child.wait().await {
            Ok(status) => {
                on_wait();
                status
            }
            Err(_) => {
                // Unknown OS termination: keep Child/its wait handle and all
                // borrowed ownership here forever, without polling a busy loop.
                std::future::pending::<std::process::ExitStatus>().await
            }
        }
    };
    let (written, out_eof, err_ok, status) = tokio::join!(write_image(stdin, image, prefix, header), stdout(out, &mut parser), stderr(err), wait);
    let complete = written.is_ok() && out_eof && err_ok && parser.complete();
    let reply = RemoteUpdateReply {
        code: status.code(),
        hash: (complete && parser.confirmed()).then(|| image.hash.to_owned()),
        stage: parser.stage,
        response_complete: complete,
        local_settled: true,
        facts: parser.facts,
        before_start_refusal: None,
    };
    ExchangeResult { reply, observed: parser.observed, failure: parser.inspect_failure }
}

#[cfg(test)]
pub(super) use test_access::*;
#[cfg(test)]
mod test_access {
    use super::*;
    // Injection is private to the test module tree, never a product ENV/CLI/API.
    pub(in crate::helpers::remote_update) fn transport(
        factory: impl Fn(&Node) -> io::Result<Command> + Send + Sync + 'static,
        context: SamplerHash,
    ) -> OwnedSshUpdateTransport {
        OwnedSshUpdateTransport { command: Box::new(factory), context, glue: b"inert-owned-transport-fixture\n", native: None }
    }
    pub(in crate::helpers::remote_update) async fn execute(transport: &OwnedSshUpdateTransport, node: &Node, image: &Image<'_>) -> RemoteUpdateReply {
        transport.execute_image(node, image).await.reply
    }
    pub(in crate::helpers::remote_update) async fn exchange_child(child: &mut Child, image: &Image<'_>, on_wait: impl FnOnce()) -> RemoteUpdateReply {
        let (prefix, header) = image.header().unwrap();
        exchange(child, image, &prefix, &header, on_wait).await.reply
    }
    #[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(target_os = "windows", target_arch = "x86_64")))]
    pub(in crate::helpers::remote_update) async fn wire_execute(
        transport: &OwnedSshUpdateTransport,
        node: &Node,
        image: &Image<'_>,
        prefix: &str,
        header: &str,
    ) -> ExchangeResult {
        let mut command = (transport.command)(node).unwrap();
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(false);
        let mut child = command.spawn().unwrap();
        exchange(&mut child, image, prefix, header, || {}).await
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(in crate::helpers::remote_update) fn native_factory(factory: impl Fn(&Node) -> io::Result<Command> + Send + Sync + 'static) -> OwnedSshUpdateTransport {
        let mut transport = native_transport();
        transport.command = Box::new(factory);
        transport
    }
    #[cfg(any(all(target_os = "windows", target_arch = "x86_64"), all(target_os = "macos", target_arch = "aarch64")))]
    pub(in crate::helpers::remote_update) fn windows_factory(
        factory: impl Fn(&Node) -> io::Result<Command> + Send + Sync + 'static,
    ) -> OwnedSshUpdateTransport {
        let mut transport = windows_native();
        transport.command = Box::new(factory);
        transport
    }
    pub(in crate::helpers::remote_update) fn local_command_bound(command: &Command) -> Option<usize> {
        command_line_bound(command)
    }
    pub(in crate::helpers::remote_update) fn command(node: &Node, launcher: &str) -> io::Result<Command> {
        ssh_command(node, launcher)
    }
}
