#!/usr/bin/env node
import WebTorrent from "webtorrent";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const shareUrl = process.argv[2];
const targetFileId = process.argv[3];

if (!shareUrl) {
  console.error("Usage: pnpm replay:webtorrent -- <share-url> [file-id]");
  process.exit(1);
}

const normalizedBase = shareUrl.replace(/\/$/, "");
const metadataUrl = `${normalizedBase}/api/metadata?metadataVersion=1`;

function waitForDone(torrent) {
  return new Promise((resolve, reject) => {
    torrent.once("done", resolve);
    torrent.once("error", reject);
  });
}

function waitForPeer(torrent, timeoutMs = 15000) {
  return new Promise((resolve) => {
    if (torrent.numPeers > 0) {
      resolve(true);
      return;
    }

    const timeout = setTimeout(() => cleanup(false), timeoutMs);
    const onWire = () => cleanup(true);

    function cleanup(result) {
      clearTimeout(timeout);
      torrent.off("wire", onWire);
      resolve(result);
    }

    torrent.on("wire", onWire);
  });
}

function attachLogging(label, torrent) {
  torrent.on("warning", (warning) => {
    console.warn(`[${label}] warning: ${String(warning)}`);
  });
  torrent.on("wire", () => {
    console.log(`[${label}] connected peers: ${torrent.numPeers}`);
  });
}

async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Request failed: ${response.status} ${response.statusText}`);
  }
  return response.json();
}

async function fetchTorrentBytes(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Torrent request failed: ${response.status} ${response.statusText}`);
  }
  return Buffer.from(await response.arrayBuffer());
}

const metadata = await fetchJson(metadataUrl);
const file = metadata.files.find((entry) => !targetFileId || entry.fileId === targetFileId);
if (!file) {
  throw new Error(`No matching file found in metadata for fileId=${targetFileId ?? "<first>"}`);
}

const torrentBytes = await fetchTorrentBytes(`${normalizedBase}/api/torrent/${file.fileId}`);
const tempRoot = await mkdtemp(join(tmpdir(), "mesh-p2p-webtorrent-"));
const tempA = join(tempRoot, "client-a");
const tempB = join(tempRoot, "client-b");

const clientA = new WebTorrent();
const clientB = new WebTorrent();

try {
  const torrentA = clientA.add(torrentBytes, { path: tempA });
  attachLogging("client-a", torrentA);
  console.log(`[client-a] downloading ${file.fileName}`);
  await waitForDone(torrentA);
  console.log(`[client-a] download complete, keeping seeding alive`);

  const torrentB = clientB.add(torrentBytes, { path: tempB });
  attachLogging("client-b", torrentB);
  const peerDetected = await waitForPeer(torrentB);
  await waitForDone(torrentB);

  console.log(`[client-b] download complete, peers detected: ${peerDetected ? "yes" : "no"}`);
  console.log(`[summary] file=${file.fileName} infoHash=${file.infoHash} peerDetected=${peerDetected}`);
} finally {
  await Promise.allSettled([
    new Promise((resolve) => clientA.destroy(resolve)),
    new Promise((resolve) => clientB.destroy(resolve)),
  ]);
  await rm(tempRoot, { recursive: true, force: true });
}
