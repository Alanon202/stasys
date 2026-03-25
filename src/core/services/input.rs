use std::{
    fs::OpenOptions,
    os::unix::{
        fs::OpenOptionsExt,
        io::{AsRawFd, OwnedFd},
    },
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use input::LibinputInterface;
use tokio::sync::Mutex;
use futures_util::FutureExt; // for now_or_never()
use crate::{core::manager::Manager, log::log_message};

struct InputDetection;

impl LibinputInterface for InputDetection {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open(path)
            .map(|f| f.into())
            .map_err(|_| -1)
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(fd)
    }
}

pub fn spawn_input_task(manager: Arc<Mutex<Manager>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Channel: blocking thread → async task
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let manager_clone = Arc::clone(&manager);

        // Async listener: reacts to input events
        let async_handle = tokio::spawn(async move {
            loop {
                let shutdown = {
                    let mgr = manager_clone.lock().await;
                    mgr.state.shutdown_flag.clone()
                };

                tokio::select! {
                    maybe_event = rx.recv() => {
                        if maybe_event.is_none() {
                            log_message("Input async handler channel closed");
                            break;
                        }

                        crate::core::events::handlers::handle_event(
                            &manager_clone,
                            crate::core::events::handlers::Event::InputActivity,
                        ).await;
                    }

                    _ = shutdown.notified() => break,
                }
            }

            log_message("libinput event loop shutting down...");
        });

        // Blocking thread: libinput event polling
        let manager_for_thread = Arc::clone(&manager);
        let blocking_handle = tokio::task::spawn_blocking(move || {
            silence_stderr();

            let mut libinput = input::Libinput::new_with_udev(InputDetection);
            if let Err(e) = libinput.udev_assign_seat("seat0") {
                eprintln!("Failed to assign seat: {:?}", e);
                return;
            }

            let fd = libinput.as_raw_fd();
            let mut last_reset = Instant::now();
            const DEBOUNCE: Duration = Duration::from_millis(300);

            loop {
                // Check for shutdown signal without blocking
                if let Ok(mgr) = manager_for_thread.try_lock() {
                    if mgr.state.shutdown_flag.notified().now_or_never().is_some() {
                        eprintln!("Input thread shutting down...");
                        break;
                    }
                }

                // Poll with 10-second timeout
                let mut pollfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                let poll_result = unsafe { libc::poll(&mut pollfd as *mut libc::pollfd, 1, 10000) };
                if poll_result < 0 {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }

                // Dispatch events once per poll
                if libinput.dispatch().is_err() {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }

                // Process events and check if any are actual input activity
                let mut has_real_input = false;
                while let Some(event) = libinput.next() {
                    // Only count these as real input activity
                    match event {
                        input::Event::Keyboard(_) | 
                        input::Event::Pointer(_) | 
                        input::Event::Touch(_) | 
                        input::Event::Tablet(_) | 
                        input::Event::Gesture(_) | 
                        input::Event::Switch(_) => {
                            has_real_input = true;
                        }
                        // Ignore device add/remove and other non-input events
                        input::Event::Device(_) => {
                            // Device add/remove - not real input
                        }
                        _ => {
                            // Other events - not real input
                        }
                    }
                }

                // Only trigger reset if we had actual input
                if has_real_input {
                    let now = Instant::now();
                    if now.duration_since(last_reset) >= DEBOUNCE {
                        last_reset = now;
                        let _ = tx.send(()); // Notify async task
                    }
                }
            }
        });

        // Wait for both to finish
        let _ = tokio::join!(async_handle, blocking_handle);
    })
}

fn silence_stderr() {
    if let Ok(dev_null) = OpenOptions::new().write(true).open("/dev/null") {
        unsafe { libc::dup2(dev_null.as_raw_fd(), libc::STDERR_FILENO) };
    }
}
