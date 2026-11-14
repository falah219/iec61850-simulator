//! Proxy TCP per IED (PLAN 3.3): port publik -> IedServer di `127.0.0.1:<internal>`.
//! Menghitung client dan memetakan koneksi internal ke alamat client asli (untuk log kontrol & RCB).
//! Skenario hang / putus / delay ditambahkan di fase 7.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use crate::events::Kind;
use crate::server::{now_ms, IedCtx};

#[derive(Debug, Clone)]
pub struct Client {
    /// alamat client asli (mis. FEP)
    pub peer: SocketAddr,
    /// port lokal koneksi proxy -> IedServer (= port peer yang dilihat libIEC61850)
    pub internal_port: u16,
    pub since_ms: u64,
}

pub async fn run(listener: TcpListener, internal: u16, ctx: Arc<IedCtx>) {
    // JoinSet: koneksi ikut dihentikan saat task proxy di-abort (IED dihapus / dimatikan)
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            r = listener.accept() => match r {
                Ok((sock, peer)) => {
                    conns.spawn(forward(sock, peer, internal, Arc::clone(&ctx)));
                }
                Err(e) => ctx.event(Kind::Connection, format!("accept gagal: {e}")),
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

async fn forward(mut sock: TcpStream, peer: SocketAddr, internal: u16, ctx: Arc<IedCtx>) {
    let mut up = match TcpStream::connect(("127.0.0.1", internal)).await {
        Ok(s) => s,
        Err(e) => return ctx.event(Kind::Connection, format!("client {peer} ditolak: IED internal tidak bisa dihubungi ({e})")),
    };
    let internal_port = up.local_addr().map_or(0, |a| a.port());
    let _ = sock.set_nodelay(true);
    let _ = up.set_nodelay(true);
    ctx.add_client(Client { peer, internal_port, since_ms: now_ms() });
    ctx.event(Kind::Connection, format!("client konek {peer} (jumlah {})", ctx.client_count()));

    let res = copy_bidirectional(&mut sock, &mut up).await;
    ctx.remove_client(internal_port);
    let detail = match res {
        Ok((tx, rx)) => format!("{tx} byte ke IED, {rx} byte ke client"),
        Err(e) => e.to_string(),
    };
    ctx.event(Kind::Connection, format!("client putus {peer} ({detail}, jumlah {})", ctx.client_count()));
}
