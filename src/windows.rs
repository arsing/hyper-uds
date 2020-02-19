pub struct UnixStream {
	inner: std::sync::Arc<Socket>,

	pending_read: std::sync::Arc<std::sync::Mutex<PendingRead>>,
	reader_sender: std::sync::mpsc::Sender<std::task::Waker>,

	pending_write: std::sync::Arc<std::sync::Mutex<PendingWrite>>,
	flush_sender: std::sync::mpsc::Sender<std::task::Waker>,
}

struct PendingRead {
	bytes: bytes::BytesMut,
	err: Option<std::io::Error>,
	eof: bool,
}

struct PendingWrite {
	bytes: bytes::BytesMut,
	flushed: bool,
	err: Option<std::io::Error>,
	eof: bool,
}

impl std::fmt::Debug for UnixStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("UnixStream").finish()
	}
}

impl UnixStream {
	pub async fn connect<P>(path: P) -> std::io::Result<Self> where P: AsRef<std::path::Path> + Send + 'static {
		let join_handle = tokio::task::spawn_blocking(|| Socket::connect(path));
		let inner = join_handle.await??;

		let result = inner.into();
		Ok(result)
	}
}

impl From<Socket> for UnixStream {
	fn from(inner: Socket) -> Self {
		let inner = std::sync::Arc::new(inner);

		let pending_read = std::sync::Arc::new(std::sync::Mutex::new(PendingRead {
			bytes: bytes::BytesMut::new(),
			err: None,
			eof: false,
		}));

		let (reader_sender, reader_receiver) = std::sync::mpsc::channel::<std::task::Waker>();
		let _ = std::thread::spawn({
			let inner = inner.clone();
			let pending_read = pending_read.clone();

			move || -> std::io::Result<()> {
				let mut buf = vec![0_u8; 1024];

				while let Ok(waker) = reader_receiver.recv() {
					match pending_read.lock() {
						Ok(pending_read) => {
							let pending_read = &*pending_read;

							if !pending_read.bytes.is_empty() || pending_read.eof || pending_read.err.is_some() {
								waker.wake();
								continue;
							}
						},

						Err(_) => break,
					}

					let read = std::io::Read::read(&mut &*inner, &mut buf);

					match pending_read.lock() {
						Ok(mut pending_read) => {
							let pending_read = &mut *pending_read;

							match read {
								Ok(0) => pending_read.eof = true,
								Ok(read) => pending_read.bytes.extend_from_slice(&buf[..read]),
								Err(err) => {
									pending_read.err = Some(err);
									pending_read.eof = true;
								},
							}

							waker.wake();
						},

						Err(_) => break,
					}
				}

				Ok(())
			}
		});


		let pending_write = std::sync::Arc::new(std::sync::Mutex::new(PendingWrite {
			bytes: bytes::BytesMut::new(),
			flushed: true,
			err: None,
			eof: false,
		}));

		let (flush_sender, flush_receiver) = std::sync::mpsc::channel::<std::task::Waker>();
		let _ = std::thread::spawn({
			let inner = inner.clone();
			let pending_write = pending_write.clone();

			move || -> std::io::Result<()> {
				'outer: while let Ok(waker) = flush_receiver.recv() {
					loop {
						let pending_write_bytes = match pending_write.lock() {
							Ok(mut pending_write) => {
								let pending_write = &mut *pending_write;

								if !pending_write.bytes.is_empty() && !pending_write.eof && pending_write.err.is_none() {
									Some(pending_write.bytes.split())
								}
								else {
									None
								}
							},

							Err(_) => break 'outer,
						};

						let result =
							if let Some(pending_write_bytes) = pending_write_bytes {
								let result = std::io::Write::write_all(&mut &*inner, &*pending_write_bytes)
								.and_then(|_| std::io::Write::flush(&mut &*inner));
								result
							}
							else {
								Ok(())
							};

						match pending_write.lock() {
							Ok(mut pending_write) => {
								let pending_write = &mut *pending_write;

								match result {
									Ok(()) =>
										if pending_write.bytes.is_empty() {
											pending_write.flushed = true
										}
										else {
											// More pending writes happened while we were flushing
											continue;
										},

									Err(err) => {
										pending_write.err = Some(err);
										pending_write.eof = true;
									},
								}
							},

							Err(_) => break 'outer,
						}

						waker.wake();
						break;
					}
				}

				Ok(())
			}
		});

		UnixStream {
			inner,

			pending_read,
			reader_sender,

			pending_write,
			flush_sender,
		}
	}
}

impl Drop for UnixStream {
	fn drop(&mut self) {
		let _ = self.inner.shutdown();
	}
}

impl tokio::io::AsyncRead for UnixStream {
	fn poll_read(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut [u8]) -> std::task::Poll<std::io::Result<usize>> {
		match self.pending_read.lock() {
			Ok(mut pending_read) => {
				let pending_read = &mut *pending_read;

				if !pending_read.bytes.is_empty() {
					let len = std::cmp::min(pending_read.bytes.len(), buf.len());
					buf[..len].copy_from_slice(&pending_read.bytes[..len]);
					bytes::Buf::advance(&mut pending_read.bytes, len);
					std::task::Poll::Ready(Ok(len))
				}
				else if let Some(err) = pending_read.err.take() {
					std::task::Poll::Ready(Err(err))
				}
				else if pending_read.eof {
					std::task::Poll::Ready(Ok(0))
				}
				else {
					match self.reader_sender.send(cx.waker().clone()) {
						Ok(()) => std::task::Poll::Pending,
						Err(_) => std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, "reader receiver dropped"))),
					}
				}
			},

			Err(_) => std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, "reader mutex poisoned"))),
		}
	}
}

impl tokio::io::AsyncWrite for UnixStream {
	fn poll_write(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
		match self.pending_write.lock() {
			Ok(mut pending_write) => {
				let pending_write = &mut *pending_write;

				if let Some(err) = pending_write.err.take() {
					std::task::Poll::Ready(Err(err))
				}
				else if pending_write.eof {
					std::task::Poll::Ready(Ok(0))
				}
				else {
					pending_write.bytes.extend_from_slice(buf);
					pending_write.flushed = false;
					std::task::Poll::Ready(Ok(buf.len()))
				}
			},

			Err(_) => std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, "writer mutex poisoned"))),
		}
	}

	fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
		match self.pending_write.lock() {
			Ok(mut pending_write) => {
				let pending_write = &mut *pending_write;

				if let Some(err) = pending_write.err.take() {
					std::task::Poll::Ready(Err(err))
				}
				else if pending_write.flushed {
					std::task::Poll::Ready(Ok(()))
				}
				else {
					match self.flush_sender.send(cx.waker().clone()) {
						Ok(()) => std::task::Poll::Pending,
						Err(_) => std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, "flush receiver dropped"))),
					}
				}
			},

			Err(_) => std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, "writer mutex poisoned"))),
		}
	}

	fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
		match self.as_mut().poll_flush(cx) {
			std::task::Poll::Ready(Ok(())) => (),
			std::task::Poll::Ready(Err(err)) => return std::task::Poll::Ready(Err(err)),
			std::task::Poll::Pending => return std::task::Poll::Pending,
		}

		let result = self.inner.shutdown();
		std::task::Poll::Ready(result)
	}
}

impl crate::UdsIncoming {
	pub fn bind<P>(path: P) -> std::io::Result<Self> where P: AsRef<std::path::Path> {
		let inner = Socket::bind(path)?;

		let accept_stream = Box::pin(futures_util::stream::unfold(inner, move |mut inner| async {
			loop {
				let join_handle = tokio::task::spawn_blocking(move || {
					match inner.accept() {
						Ok(socket) => (Ok(socket), inner),
						Err(err) => (Err(err), inner),
					}
				});
				let (socket, inner_) = match join_handle.await {
					Ok((Ok(socket), inner)) => (Some(socket), inner),
					Ok((Err(_), inner)) => (None, inner),
					Err(_) => panic!("bound socket is lost due to spawn failure"),
				};

				if let Some(socket) = socket {
					return Some((socket.into(), inner_));
				}

				inner = inner_;
			}
		}));

		Ok(crate::UdsIncoming {
			accept_stream,
		})
	}
}

#[derive(Debug)]
struct Socket(winapi::um::winsock2::SOCKET);

impl Socket {
	fn new() -> std::io::Result<Self> {
		unsafe {
			let socket =
				winapi::um::winsock2::WSASocketW(
					winapi::shared::ws2def::AF_UNIX,
					winapi::um::winsock2::SOCK_STREAM,
					0,
					std::ptr::null_mut(),
					0,
					0,
				);
			if socket == winapi::um::winsock2::INVALID_SOCKET {
				return Err(winsock_error());
			}

			let socket = Socket(socket);

			socket.set_no_inherit()?;

			Ok(socket)
		}
	}

	fn connect<P>(path: P) -> std::io::Result<Self> where P: AsRef<std::path::Path> {
		unsafe {
			init_winsock();

			let socket = Socket::new()?;
			let (addr, len) = sockaddr_un::new(path.as_ref())?;

			let result =
				winapi::um::winsock2::connect(
					socket.0,
					&addr as *const _ as *const _,
					len,
				);
			if result != 0 {
				return Err(winsock_error());
			}

			Ok(socket)
		}
	}

	fn bind<P>(path: P) -> std::io::Result<Self> where P: AsRef<std::path::Path> {
		unsafe {
			init_winsock();

			let socket = Socket::new()?;
			let (addr, len) = sockaddr_un::new(path.as_ref())?;

			let result =
				winapi::um::winsock2::bind(
					socket.0,
					&addr as *const _ as *const _,
					len,
				);
			if result != 0 {
				return Err(winsock_error());
			}

			let result =
				winapi::um::winsock2::listen(
					socket.0,
					128,
				);
			if result != 0 {
				return Err(winsock_error());
			}

			Ok(socket)
		}
	}

	fn accept(&self) -> std::io::Result<Self> {
		unsafe {
			let socket =
				winapi::um::winsock2::accept(
					self.0,
					std::ptr::null_mut(),
					std::ptr::null_mut(),
				);
			if socket == winapi::um::winsock2::INVALID_SOCKET {
				return Err(winsock_error());
			}

			let socket = Socket(socket);

			socket.set_no_inherit()?;

			Ok(socket)
		}
	}

	fn set_no_inherit(&self) -> std::io::Result<()> {
		unsafe {
			let result =
				winapi::um::handleapi::SetHandleInformation(
					self.0 as winapi::um::winnt::HANDLE,
					winapi::um::winbase::HANDLE_FLAG_INHERIT,
					0,
				);
			if result == 0 {
				return Err(std::io::Error::last_os_error());
			}

			Ok(())
		}
	}

	fn shutdown(&self) -> std::io::Result<()> {
		unsafe {
			let result = winapi::um::winsock2::shutdown(
				self.0,
				winapi::um::winsock2::SD_BOTH,
			);
			if result != 0 {
				return Err(winsock_error());
			}

			Ok(())
		}
	}
}

impl Drop for Socket {
	fn drop(&mut self) {
		unsafe {
			let _ = winapi::um::winsock2::closesocket(self.0);
		}
	}
}

impl std::io::Read for &'_ Socket {
	fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
		// Winsock appears to have a bug where if a recv() is in progress, then another thread running send() will block indefinitely.
		//
		// So we cannot just have this thread call recv() and block on it. Imagine this is an HTTP client that wants to send() an HTTP request,
		// and will not have anything to recv() until it has send()'d a request and the server responds.
		//
		// So what if we applied a timeout to the recv() so that it would not block indefinitely, and then std::thread::sleep()'d for some time to let the send() go through?
		// That doesn't work either, because SO_RCVTIMEO appears to be ignored for Unix sockets.
		//
		// So what if we used select() to wait for the socket to be readable? Since that doesn't count as an actual read, it should not block a send() from going through, right?
		// Nope, that also blocks the send().
		//
		// So the last resort is a combination of these two methods - use select() to wait for the socket to be readable, but also apply a timeout and sleep so that the send()
		// has a chance to go through between select()s. Timeout does work for select(), unlike SO_RCVTIMEO for recv().

		unsafe {
			loop {
				let mut fd_array = [0; 64];
				fd_array[0] = self.0;

				let mut read_set = winapi::um::winsock2::fd_set {
					fd_count: 1,
					fd_array,
				};

				let result =
					winapi::um::winsock2::select(
						0,
						&mut read_set,
						std::ptr::null_mut(),
						std::ptr::null_mut(),
						&winapi::um::winsock2::TIMEVAL {
							tv_sec: 0,
							tv_usec: 0,
						},
					);
				if result < 0 {
					return Err(winsock_error());
				}

				if read_set.fd_count > 0 {
					let result = winapi::um::winsock2::recv(
						self.0,
						buf.as_mut_ptr() as *mut winapi::ctypes::c_char,
						std::convert::TryInto::try_into(buf.len()).expect("usize -> c_int"),
						0,
					);
					if result < 0 {
						return Err(winsock_error());
					}

					let result = std::convert::TryInto::try_into(result).expect("c_int -> usize");
					return Ok(result);
				}

				std::thread::sleep(std::time::Duration::from_secs(1));
			}
		}
	}
}

impl std::io::Write for &'_ Socket {
	fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
		unsafe {
			let result = winapi::um::winsock2::send(
				self.0,
				buf.as_ptr() as *const winapi::ctypes::c_char,
				std::convert::TryInto::try_into(buf.len()).expect("usize -> c_int"),
				0,
			);
			if result < 0 {
				return Err(winsock_error());
			}

			Ok(std::convert::TryInto::try_into(result).expect("c_int -> usize"))
		}
	}

	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
	}
}

unsafe fn init_winsock() {
	static INIT: std::sync::Once = std::sync::Once::new();

	INIT.call_once(|| {
		let mut data: winapi::um::winsock2::WSADATA = std::mem::zeroed();
		let result =
			winapi::um::winsock2::WSAStartup(
				0x202, // version 2.2
				&mut data,
			);
		assert_eq!(result, 0);

		// Don't bother with WSACleanup
	});
}

unsafe fn winsock_error() -> std::io::Error {
	std::io::Error::from_raw_os_error(winapi::um::winsock2::WSAGetLastError())
}

#[allow(non_camel_case_types)]
#[repr(C)]
pub struct sockaddr_un {
	pub sun_family: winapi::shared::ws2def::ADDRESS_FAMILY,
	pub sun_path: [winapi::um::winnt::CHAR; 108],
}

impl sockaddr_un {
	unsafe fn new(path: &std::path::Path) -> std::io::Result<(Self, std::os::raw::c_int)> {
		let mut addr: Self = std::mem::zeroed();
		addr.sun_family = std::convert::TryInto::try_into(winapi::shared::ws2def::AF_UNIX).expect("pre-defined constant");

		// Winsock2 expects 'sun_path' to be a Win32 UTF-8 file system path
		let path =
			path.to_str()
			.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path must be utf-8"))?
			.as_bytes();
		if path.contains(&0) {
			return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains interior null byte"));
		}
		let path_len = path.len();
		if path_len >= addr.sun_path.len() {
			return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "path must be shorter than SUN_LEN"));
		}
		{
			// Reinterpret &[u8] as &[i8]
			let path: &[winapi::um::winnt::CHAR] = std::slice::from_raw_parts(path.as_ptr() as *const _, path.len());

			addr.sun_path[..path_len].copy_from_slice(path);
		}

		// addr.sun_path is already null-terminated because addr was created with std::mem::zeroed()

		let sun_path_offset = {
			let base_addr = &addr as *const _ as usize;
			let sun_path_addr = &addr.sun_path as *const _ as usize;
			sun_path_addr - base_addr
		};
		let len = sun_path_offset + path_len + 1;
		let len = std::convert::TryInto::try_into(len).expect("usize -> c_int");

		Ok((addr, len))
	}
}
