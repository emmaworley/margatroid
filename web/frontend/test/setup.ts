/**
 * Test fixture: starts a throwaway relay+bash session for tests to use,
 * so keystrokes don't go to the live Claude session.
 */
import { execSync, spawn, type ChildProcess } from "child_process";
import { existsSync, mkdirSync, rmSync } from "fs";
import { join } from "path";

const SESSION_NAME = "_test-session";
const MARGATROID_DIR = process.env.MARGATROID_DIR ??
  join(process.env.HOME!, ".margatroid");
const SESSION_DIR = join(MARGATROID_DIR, "sessions", SESSION_NAME);
const SOCK_PATH = join(SESSION_DIR, "relay.sock");

let relayProcess: ChildProcess | null = null;

function findRelayBin(): string {
  const installed = join(MARGATROID_DIR, "bin", "margatroid-relay");
  if (existsSync(installed)) return installed;

  // Dev build
  const dev = join(__dirname, "..", "..", "..", "target", "debug", "margatroid-relay");
  if (existsSync(dev)) return dev;

  // Search relative to repo root
  const repo = join(__dirname, "..", "..", "..", "..");
  const devAlt = join(repo, "target", "debug", "margatroid-relay");
  if (existsSync(devAlt)) return devAlt;

  throw new Error("margatroid-relay binary not found");
}

export function startTestRelay(): void {
  mkdirSync(SESSION_DIR, { recursive: true });

  // Clean stale socket
  try { rmSync(SOCK_PATH); } catch {}

  const relayBin = findRelayBin();
  relayProcess = spawn(relayBin, [SESSION_NAME, "/bin/bash", "--norc", "--noprofile"], {
    stdio: "ignore",
    detached: true,
    env: { ...process.env, TERM: "xterm-256color" },
  });
  relayProcess.unref();

  // Wait for socket to appear
  const deadline = Date.now() + 5000;
  while (!existsSync(SOCK_PATH) && Date.now() < deadline) {
    execSync("sleep 0.1");
  }
  if (!existsSync(SOCK_PATH)) {
    throw new Error("relay socket did not appear");
  }
}

export function stopTestRelay(): void {
  if (relayProcess?.pid) {
    try { process.kill(-relayProcess.pid, "SIGTERM"); } catch {}
    try { process.kill(relayProcess.pid, "SIGTERM"); } catch {}
  }
  relayProcess = null;
  try { rmSync(SESSION_DIR, { recursive: true }); } catch {}
}

export const TEST_SESSION = SESSION_NAME;
