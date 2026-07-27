// Handing bytes from the Rust/wasm playground to the browser as a file download.
//
// Pulled in as a wasm-bindgen snippet, like `monaco_glue.js`. Keeping the DOM
// work here rather than in Rust means the crate needs no extra `web-sys`
// features and nothing on the compile path can reach a browser API.

// Offer `bytes` to the user as a file named `name`.
//
// The `Blob` constructor copies out of the wasm heap synchronously, so the
// caller's view stays valid for exactly as long as it needs to. The object URL
// is revoked on the next macrotask: revoking it in the same turn as the click
// races the browser's own fetch of it in some engines.
export function downloadBytes(name, bytes) {
  const url = URL.createObjectURL(
    new Blob([bytes], { type: "application/octet-stream" }),
  );
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  // Firefox only dispatches the click for an anchor in the document.
  anchor.style.display = "none";
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
