//! Proxy TCP per IED (PLAN 3.3): port publik -> IedServer di `127.0.0.1:<internal>`.
//! Menghitung client, memetakan koneksi internal ke alamat client asli (untuk log kontrol & RCB),
//! dan menjalankan skenario jaringan tanpa mengubah libIEC61850:
//! - hang  : berhenti meneruskan data, TCP tetap terbuka
//! - delay : jeda N ms per potongan data yang diteruskan
//! - putus : semua koneksi client ditutup (FIN, atau RST dengan mode rst), IED tetap jalan
//! (IED mati = task proxy di-abort: listener + semua koneksi tertutup.)

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
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

/// State jaringan satu IED, dibagikan ke semua koneksi lewat `watch`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Net {
    pub hang: bool,
    pub delay_ms: u64,
    /// naik 1 setiap skenario "putus koneksi"; koneksi yang melihat perubahan menutup diri
    pub epoch: u64,
    /// putus koneksi terakhir pakai RST (SO_LINGER 0) bukan FIN
    pub rst: bool,
}

pub async fn run(listener: TcpListener, internal: u16, ctx: Arc<IedCtx>) {
    // JoinSet: koneksi ikut ditutup saat task proxy di-abort (IED mati / dihapus)
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

    let mut net = ctx.net.subscribe();
    let epoch = net.borrow().epoch;
    let (mut detail, cut) = {
        let (cr, cw) = sock.split();
        let (ur, uw) = up.split();
        tokio::select! {
            r = async { tokio::try_join!(pump(cr, uw, ctx.net.subscribe()), pump(ur, cw, ctx.net.subscribe())) } => match r {
                Ok((tx, rx)) => (format!("{tx} byte ke IED, {rx} byte ke client"), false),
                Err(e) => (e.to_string(), false),
            },
            _ = net.wait_for(|n| n.epoch != epoch) => ("diputus oleh skenario".to_string(), true),
        }
    };
    if cut && net.borrow().rst {
        // SO_LINGER 0: close() mengirim RST, bukan FIN
        #[allow(deprecated)]
        let _ = sock.set_linger(Some(Duration::ZERO));
        detail.push_str(", RST");
    }
    ctx.remove_client(internal_port);
    ctx.event(Kind::Connection, format!("client putus {peer} ({detail}, jumlah {})", ctx.client_count()));
}

/// Satu arah: baca -> (tahan selama hang) -> (jeda delay) -> tulis.
async fn pump(mut r: impl AsyncReadExt + Unpin, mut w: impl AsyncWriteExt + Unpin, mut net: watch::Receiver<Net>) -> std::io::Result<u64> {
    let mut buf = vec![0u8; 16 * 1024];
    let mut total = 0;
    loop {
        // selama hang tidak membaca: data menumpuk di buffer TCP, koneksi tetap terbuka
        let _ = net.wait_for(|n| !n.hang).await;
        let n = r.read(&mut buf).await?;
        if n == 0 {
            let _ = w.shutdown().await;
            return Ok(total);
        }
        let delay = net.borrow().delay_ms;
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        let _ = net.wait_for(|n| !n.hang).await; // hang dimulai saat data sudah terbaca: tahan dulu
        w.write_all(&buf[..n]).await?;
        total += n as u64;
    }
}
