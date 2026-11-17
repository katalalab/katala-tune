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

type CommandFactory = dyn Fn(&Node) -> io::Result<Command> + Send + Sync;
/// Opaque until reviewed native inspect/placement binds Node/account/canonical
/// destination to context and supplies fixed glue. There is no production constructor.
pub struct OwnedSshUpdateTransport {
    command: Box<CommandFactory>,
    context: SamplerHash,
    glue: &'static [u8],
}
impl RemoteUpdateTransport for OwnedSshUpdateTransport {
    fn place<'a>(&'a self, node: &'a Node, payload: RemoteUpdatePayload<'a>) -> RemoteUpdateFuture<'a> {
        Box::pin(async move {
            let image = Image::payload(&payload, self.context, self.glue);
            self.execute(node, &image).await
        })
    }
}
impl OwnedSshUpdateTransport {
    async fn execute(&self, node: &Node, image: &Image<'_>) -> RemoteUpdateReply {
        let Some((prefix, header)) = image.header().filter(|_| valid_alias(&node.alias)) else {
            return unknown();
        };
        let Ok(mut command) = (self.command)(node) else {
            return unknown();
        };
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(false);
        let Ok(mut child) = command.spawn() else {
            return unknown();
        };
        // No owned branch is detached or canceled by an error in another branch.
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
/// Future native renderer calls this private builder with its fixed launcher.
/// Never invoke the legacy timeout/kill runner. Options are built from scratch:
/// OpenSSH uses the first value, so appending over SSH_OPTS is not an override.
#[cfg_attr(not(test), expect(dead_code, reason = "production native launcher is deferred to B/C"))]
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
    command.arg(&node.alias).arg(collect::remote_command(node, fixed_launcher));
    Ok(command)
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
async fn exchange(child: &mut Child, image: &Image<'_>, prefix: &str, header: &str, on_wait: impl FnOnce()) -> RemoteUpdateReply {
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
    RemoteUpdateReply {
        code: status.code(),
        hash: (complete && parser.confirmed()).then(|| image.hash.to_owned()),
        stage: parser.stage,
        response_complete: complete,
        local_settled: true,
        facts: parser.facts,
    }
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
        OwnedSshUpdateTransport { command: Box::new(factory), context, glue: b"inert-owned-transport-fixture\n" }
    }
    pub(in crate::helpers::remote_update) async fn execute(transport: &OwnedSshUpdateTransport, node: &Node, image: &Image<'_>) -> RemoteUpdateReply {
        transport.execute(node, image).await
    }
    pub(in crate::helpers::remote_update) async fn exchange_child(child: &mut Child, image: &Image<'_>, on_wait: impl FnOnce()) -> RemoteUpdateReply {
        let (prefix, header) = image.header().unwrap();
        exchange(child, image, &prefix, &header, on_wait).await
    }
    pub(in crate::helpers::remote_update) fn command(node: &Node, launcher: &str) -> io::Result<Command> {
        ssh_command(node, launcher)
    }
}
