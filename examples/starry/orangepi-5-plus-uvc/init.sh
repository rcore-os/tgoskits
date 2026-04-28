RUST_BACKTRACE=1 LD_LIBRARY_PATH="/usr/local/lib:/usr/lib:/lib:${LD_LIBRARY_PATH:-}" sh -c 'echo STARRY_UVC_FPS_BEGIN; exec /usr/bin/uvc-fps --device 0 --format mjpeg --interval-sec 1'
