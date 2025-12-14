mod network;

use arboard::Clipboard;
use clap::Parser;
use clipboard_master::{CallbackResult, ClipboardHandler, Master};
use env_logger::Env;
use log::{debug, error};
use std::io;
use tokio::sync::{mpsc, oneshot};
struct Handler {
    sender: mpsc::Sender<String>,
}

impl ClipboardHandler for Handler {
    fn on_clipboard_change(&mut self) -> CallbackResult {
        debug!("Clipboard change happened!");
        get_clipboard_content(self.sender.clone());
        CallbackResult::Next
    }

    fn on_clipboard_error(&mut self, error: io::Error) -> CallbackResult {
        error!("Error: {}", error);
        CallbackResult::Next
    }

    fn sleep_interval(&self) -> core::time::Duration {
        core::time::Duration::from_millis(1000)
    }
}

#[derive(Parser, Debug)]
#[command(version = env!("CARGO_PKG_VERSION"), author = env!("CARGO_PKG_AUTHORS"), about = env!("CARGO_PKG_DESCRIPTION"))]
struct Args {
    /// The remote peer to connect to on boot up.
    #[arg(short, long, num_args = 2, value_names = ["IP:PORT", "PEER_ID"])]
    connect: Option<Vec<String>>,
    /// Path to custom private key. The key should be an ED25519 private key in PEM format.
    #[arg(short, long, value_name = "PATH")]
    key: Option<String>,
    /// Local address to listen on.
    #[arg(short, long, value_name = "IP:PORT")]
    listen: Option<String>,
    /// Pre-shared key. Only nodes with same key can connect to each other.
    #[arg(short, long)]
    psk: Option<String>,
    /// If set, no mDNS broadcasts will be made.
    #[arg(short, long)]
    no_mdns: bool,
}

fn get_clipboard_content(sender: mpsc::Sender<String>) {
    let mut ctx = match Clipboard::new() {
        Ok(context) => context,
        Err(err) => {
            error!("Error creating ClipboardContext: {}", err);
            return;
        }
    };

    // TODO: handle non-text contents
    match ctx.get_text() {
        Ok(contents) => {
            sender.try_send(contents).unwrap();
        }
        Err(err) => error!("Error getting clipboard contents: {}", err),
    }
}

fn set_clipboard_content(content: &str) {
    let mut ctx = match Clipboard::new() {
        Ok(context) => context,
        Err(err) => {
            error!("Error creating ClipboardContext: {}", err);
            return;
        }
    };
    let _ = ctx.set_text(content);
}

fn create_clipboard_monitor(sender: mpsc::Sender<String>) -> Master<Handler> {
    let handler = Handler { sender };
    let master = Master::new(handler);
    return master.unwrap();
}

async fn channel_proxy(mut rx: mpsc::Receiver<String>, mut shutdown: oneshot::Receiver<()>) {
    loop {
        tokio::select! {
             Some(message) = rx.recv() => {
                set_clipboard_content(message.as_ref());
                debug!("Proxy received: {}", message);
            },
            _ = &mut shutdown => {
                debug!("Proxy shutdown received");
                return;
            },
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stdout)
        .init();
    let Args {
        connect,
        key,
        listen,
        psk,
        no_mdns,
    } = Args::parse();
    loop {
        // clipboard tx channel
        let (from_clipboard_tx, from_clipboard_rx) = mpsc::channel::<String>(32);
        // clipboard rx channel
        let (to_clipboard_tx, to_clipboard_rx) = mpsc::channel::<String>(32);
        let (shutdown_proxy_tx, shutdown_proxy_rx) = oneshot::channel::<()>();
        // We cannot move handlers on Windows because *mut c_void cannot be moved. Create a channel to capture the shutdown channel.
        let (shutdown_channel_tx, shutdown_channel_rx) = oneshot::channel();
        let _ = tokio::spawn(channel_proxy(to_clipboard_rx, shutdown_proxy_rx));
        // Clipboard functionality is fully synchronous, so it is impossible to have it integrated in tokio runtime as it is.
        // We have to start a dedicated thread instead of run it in tokio runtime.
        std::thread::spawn(move || {
            let mut monitor = create_clipboard_monitor(from_clipboard_tx);
            let shutdown = monitor.shutdown_channel();
            let _ = shutdown_channel_tx.send(shutdown);
            monitor.run()
        });
        let result = network::start_network(
            from_clipboard_rx,
            to_clipboard_tx,
            connect.clone(),
            key.clone(),
            listen.clone(),
            psk.clone(),
            no_mdns,
        )
        .await;
        if let Err(error_in_network) = result {
            error!("Fatal Error: {}", error_in_network);
            std::process::exit(1);
        }
        shutdown_channel_rx.await.unwrap().signal();
        let _ = shutdown_proxy_tx.send(());
    }
}
