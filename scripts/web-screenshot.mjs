#!/usr/bin/env node
// Screenshot a URL with headless Chromium over the DevTools protocol, after a
// real-time settle — Chromium's own `--screenshot --virtual-time-budget` mode
// freezes timers once the load event fires, which stalls gpui's frame loop
// before the wasm app has laid itself out. Dependency-free: Node's built-in
// WebSocket and fetch (Node 22+).
//
//   web-screenshot.mjs <chromium> <url> <out.png> [WxH] [settle-ms]
//
// Console messages and uncaught errors from the page are echoed to stderr, so
// a Rust panic shows up in the log instead of a blank capture.

import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [chrome, url, out, sizeArg = "1200x720", settleArg = "6000"] = process.argv.slice(2);
if (!chrome || !url || !out) {
    console.error("usage: web-screenshot.mjs <chromium> <url> <out.png> [WxH] [settle-ms]");
    process.exit(2);
}
const [width, height] = sizeArg.split(/[x,]/).map(Number);
const settle = Number(settleArg);
const profile = mkdtempSync(join(tmpdir(), "pwrde-web-"));

const proc = spawn(
    chrome,
    [
        "--headless=new",
        "--no-sandbox",
        "--hide-scrollbars",
        `--window-size=${width},${height}`,
        // SwiftShader gives headless Chromium a software WebGL/WebGPU device.
        "--use-angle=swiftshader",
        "--enable-unsafe-swiftshader",
        "--enable-unsafe-webgpu",
        "--remote-debugging-port=0",
        `--user-data-dir=${profile}`,
        "about:blank",
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
);
const cleanup = () => {
    proc.kill();
    rmSync(profile, { recursive: true, force: true });
};
process.on("exit", cleanup);

// Chromium announces the DevTools endpoint on stderr once it is up.
const port = await new Promise((resolve, reject) => {
    let buf = "";
    proc.stderr.on("data", (chunk) => {
        buf += chunk;
        const m = buf.match(/DevTools listening on ws:\/\/127\.0\.0\.1:(\d+)\//);
        if (m) resolve(Number(m[1]));
    });
    proc.on("exit", (code) => reject(new Error(`chromium exited early (${code}): ${buf}`)));
    setTimeout(() => reject(new Error("chromium did not start")), 20000);
});

const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const page = targets.find((t) => t.type === "page");
if (!page) throw new Error("no page target");

const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
});
let nextId = 1;
const pending = new Map();
const loaded = new Promise((resolve) => (ws.onLoaded = resolve));
ws.onmessage = ({ data }) => {
    const msg = JSON.parse(data);
    if (msg.id && pending.has(msg.id)) {
        const { resolve, reject } = pending.get(msg.id);
        pending.delete(msg.id);
        msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    } else if (msg.method === "Page.loadEventFired") {
        ws.onLoaded();
    } else if (msg.method === "Runtime.consoleAPICalled") {
        const text = msg.params.args.map((a) => a.value ?? a.description ?? "").join(" ");
        console.error(`[console.${msg.params.type}] ${text}`);
    } else if (msg.method === "Runtime.exceptionThrown") {
        console.error(`[exception] ${msg.params.exceptionDetails.text} ${msg.params.exceptionDetails.exception?.description ?? ""}`);
    }
};
const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
        const id = nextId++;
        pending.set(id, { resolve, reject });
        ws.send(JSON.stringify({ id, method, params }));
    });

await send("Page.enable");
await send("Runtime.enable");
await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
await send("Page.navigate", { url });
await Promise.race([loaded, new Promise((r) => setTimeout(r, 30000))]);
// Let the wasm module boot, lay out, and paint a few real frames.
await new Promise((r) => setTimeout(r, settle));
const { data } = await send("Page.captureScreenshot", { format: "png" });
writeFileSync(out, Buffer.from(data, "base64"));
ws.close();
console.log(out);
process.exit(0);
