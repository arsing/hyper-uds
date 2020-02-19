#[derive(Debug)]
pub struct UnixStream {
	inner: tokio::net::UnixStream,
}

impl UnixStream {
	pub async fn connect<P>(path: P) -> std::io::Result<Self> where P: AsRef<std::path::Path> {
		let inner = tokio::net::UnixStream::connect(path).await?;
		Ok(UnixStream {
			inner,
		})
	}
}

impl tokio::io::AsyncRead for UnixStream {
	fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut [u8]) -> std::task::Poll<std::io::Result<usize>> {
		std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
	}
}

impl tokio::io::AsyncWrite for UnixStream {
	fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
		std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
	}

	fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
		std::pin::Pin::new(&mut self.inner).poll_flush(cx)
	}

	fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
		std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
	}
}

impl crate::UdsIncoming {
	pub fn bind<P>(path: P) -> std::io::Result<Self> where P: AsRef<std::path::Path> {
		let inner = tokio::net::UnixListener::bind(path)?;

		let accept_stream = Box::pin(futures_util::stream::unfold(inner, move |mut inner| async {
			loop {
				match inner.accept().await {
					Ok((stream, _)) => return Some((UnixStream { inner: stream }, inner)),
					Err(_) => (),
				}
			}
		}));

		Ok(crate::UdsIncoming {
			accept_stream,
		})
	}
}
