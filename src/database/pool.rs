use std::{
    ops::{Deref, DerefMut},
    path::Path,
};

use rusqlite::Connection;
use smol::channel::{Receiver, Sender};

/// A pool of connections to a particular SQL database.
#[derive(Clone)]
pub struct ConnPool {
    send_conn: Sender<Connection>,
    recv_conn: Receiver<Connection>,
}

impl ConnPool {
    /// Creates a new connection pool to the SQLite database at the specified path.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let (send_conn, recv_conn) = smol::channel::bounded(64);
        for _ in 0..64 {
            let conn = Connection::open(path.as_ref())?;
            conn.query_row("pragma journal_mode=WAL", [], |_| Ok(()))?;
            conn.execute("pragma synchronous=NORMAL", [])?;
            send_conn.try_send(conn).unwrap();
        }
        Ok(Self {
            send_conn,
            recv_conn,
        })
    }

    /// Gets a connection.
    pub async fn get_conn(&self) -> impl DerefMut<Target = Connection> {
        PooledConnection {
            inner: Some(self.recv_conn.recv().await.expect("wtf")),
            send_conn: self.send_conn.clone(),
        }
    }
}

/// A wrapped connection, that returns to the pool on drop.
struct PooledConnection {
    inner: Option<Connection>,
    send_conn: Sender<Connection>,
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let inner = self.inner.take().unwrap();
        let _ = self.send_conn.try_send(inner);
    }
}

impl Deref for PooledConnection {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}
