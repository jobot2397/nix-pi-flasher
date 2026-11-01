import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

type PackageHit = {
  attrName: string;
  version: string;
  description: string;
};

type Disk = {
  path: string;
  name: string;
  sizeBytes: number;
  model: string | null;
  tran: string | null;
  removable: boolean;
  mountpoints: string[];
};

type LogEvent = { line: string };
type FinishedEvent = {
  ok: boolean;
  image: string | null;
  error: string | null;
};

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  try {
    return JSON.stringify(error);
  } catch {
    return "Unknown error";
  }
}

function formatBytes(size: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = size;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  const digits = index === 0 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[index]}`;
}

function LogPane({ lines }: { lines: string[] }) {
  const ref = useRef<HTMLPreElement>(null);
  const stick = useRef(true);

  function onScroll() {
    const el = ref.current;
    if (!el) return;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 32;
  }

  useEffect(() => {
    const el = ref.current;
    if (el && stick.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [lines]);

  return (
    <pre ref={ref} className="log" onScroll={onScroll}>
      {lines.length === 0 ? "Build and flash logs appear here." : lines.join("\n")}
    </pre>
  );
}

export default function App() {
  const [enableSsh, setEnableSsh] = useState(true);
  const [setHostname, setSetHostname] = useState(true);
  const [hostname, setHostnameValue] = useState("nixpi");
  const [createUser, setCreateUser] = useState(false);
  const [username, setUsername] = useState("nixos");
  const [password, setPassword] = useState("");
  const [packages, setPackages] = useState<string[]>([]);

  const [query, setQuery] = useState("");
  const [results, setResults] = useState<PackageHit[]>([]);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);

  const [buildRunning, setBuildRunning] = useState(false);
  const [flashRunning, setFlashRunning] = useState(false);
  const [image, setImage] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [statusOk, setStatusOk] = useState(false);

  const [lines, setLines] = useState<string[]>([]);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [disks, setDisks] = useState<Disk[]>([]);
  const [diskError, setDiskError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [loadingDisks, setLoadingDisks] = useState(false);

  const requestId = useRef(0);

  function pushLine(source: "build" | "flash", line: string) {
    setLines((prev) => {
      const next = [...prev, `[${source}] ${line}`];
      return next.length > 4000 ? next.slice(next.length - 4000) : next;
    });
  }

  useEffect(() => {
    let unlisten: Array<() => void> = [];
    let cancelled = false;

    void (async () => {
      const stops = [
        await listen<LogEvent>("build-log", (event) => {
          pushLine("build", event.payload.line);
        }),
        await listen<FinishedEvent>("build-finished", (event) => {
          setBuildRunning(false);
          if (event.payload.ok && event.payload.image) {
            setImage(event.payload.image);
            setStatusOk(true);
            setStatus(`Image ready: ${event.payload.image}`);
            pushLine("build", `image: ${event.payload.image}`);
          } else {
            const message = event.payload.error ?? "Build failed";
            setStatusOk(false);
            setStatus(message);
            pushLine("build", message);
          }
        }),
        await listen<LogEvent>("flash-log", (event) => {
          pushLine("flash", event.payload.line);
        }),
        await listen<FinishedEvent>("flash-finished", (event) => {
          setFlashRunning(false);
          if (event.payload.ok) {
            setStatusOk(true);
            setStatus("Flash finished");
            pushLine("flash", "finished");
          } else {
            const message = event.payload.error ?? "Flash failed";
            setStatusOk(false);
            setStatus(message);
            pushLine("flash", message);
          }
        }),
      ];
      if (cancelled) {
        stops.forEach((stop) => stop());
        return;
      }
      unlisten = stops;
    })().catch((error: unknown) => {
      setStatusOk(false);
      setStatus(errorText(error));
    });

    void invoke<string | null>("last_image")
      .then((path) => {
        if (path) setImage(path);
      })
      .catch(() => {
        // Outside the Tauri webview there is no backend yet.
      });

    return () => {
      cancelled = true;
      unlisten.forEach((stop) => stop());
    };
  }, []);

  useEffect(() => {
    const trimmed = query.trim();
    if (trimmed.length < 2) {
      setResults([]);
      setSearching(false);
      setSearchError(null);
      return;
    }
    const handle = window.setTimeout(() => {
      const id = ++requestId.current;
      setSearching(true);
      setSearchError(null);
      void invoke<PackageHit[]>("search_packages", { query: trimmed })
        .then((hits) => {
          if (id !== requestId.current) return;
          setResults(hits);
        })
        .catch((error: unknown) => {
          if (id !== requestId.current) return;
          setResults([]);
          setSearchError(errorText(error));
        })
        .finally(() => {
          if (id === requestId.current) setSearching(false);
        });
    }, 250);
    return () => window.clearTimeout(handle);
  }, [query]);

  function addPackage(attr: string) {
    setPackages((prev) => (prev.includes(attr) ? prev : [...prev, attr]));
  }

  function removePackage(attr: string) {
    setPackages((prev) => prev.filter((item) => item !== attr));
  }

  async function onBuild() {
    setStatus(null);
    setStatusOk(false);
    setBuildRunning(true);
    try {
      await invoke("start_build", {
        settings: {
          setHostname,
          hostname,
          enableSsh,
          createUser,
          username,
          password: createUser ? password : "",
          packages,
        },
      });
    } catch (error) {
      setBuildRunning(false);
      setStatusOk(false);
      setStatus(errorText(error));
    }
  }

  async function onCancel() {
    try {
      await invoke("cancel_build");
    } catch (error) {
      setStatusOk(false);
      setStatus(errorText(error));
    }
  }

  async function refreshDisks() {
    setLoadingDisks(true);
    setDiskError(null);
    setSelected(null);
    try {
      const listed = await invoke<Disk[]>("list_disks");
      setDisks(listed);
    } catch (error) {
      setDisks([]);
      setDiskError(errorText(error));
    } finally {
      setLoadingDisks(false);
    }
  }

  async function onFlash(device: string) {
    setStatus(null);
    setStatusOk(false);
    setFlashRunning(true);
    try {
      await invoke("flash", { device, confirm: device });
    } catch (error) {
      setFlashRunning(false);
      setStatusOk(false);
      setStatus(errorText(error));
    }
  }

  const busy = buildRunning || flashRunning;

  return (
    <div className="app">
      <header className="top">
        <h1>Nix Pi Flasher</h1>
        <p>Build a NixOS SD image and write it to a removable disk.</p>
      </header>

      <div className="main">
        <div className="col">
          <section>
            <h2>Image settings</h2>
            <label className="check">
              <input
                type="checkbox"
                checked={enableSsh}
                onChange={(event) => setEnableSsh(event.currentTarget.checked)}
              />
              Enable SSH
            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={setHostname}
                onChange={(event) => setSetHostname(event.currentTarget.checked)}
              />
              Set hostname
            </label>
            <label className="field">
              Hostname
              <input
                type="text"
                value={hostname}
                spellCheck={false}
                disabled={!setHostname}
                onChange={(event) => setHostnameValue(event.currentTarget.value)}
              />
            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={createUser}
                onChange={(event) => setCreateUser(event.currentTarget.checked)}
              />
              Create default user
            </label>
            <label className="field">
              Username
              <input
                type="text"
                value={username}
                spellCheck={false}
                autoComplete="off"
                disabled={!createUser}
                onChange={(event) => setUsername(event.currentTarget.value)}
              />
            </label>
            <label className="field">
              Password
              <input
                type="password"
                value={password}
                autoComplete="new-password"
                disabled={!createUser}
                onChange={(event) => setPassword(event.currentTarget.value)}
              />
            </label>
            <p className="hint">
              The password is hashed locally and is not stored in the clear.
            </p>
          </section>

          <section className="packages">
            <h2>Packages</h2>
            <p className="hint">
              Package names come from the pinned nixos-unstable nixpkgs
              (search.nixos.org channel unstable).
            </p>
            <input
              type="search"
              value={query}
              spellCheck={false}
              placeholder="Search packages"
              onChange={(event) => setQuery(event.currentTarget.value)}
            />
            {searching ? <p className="hint">Searching…</p> : null}
            {searchError ? <p className="error">{searchError}</p> : null}
            <div className="results">
              {results.map((hit) => (
                <div className="result" key={hit.attrName}>
                  <div>
                    <div className="attr">{hit.attrName}</div>
                    <div className="meta">
                      {hit.version || "unknown version"}
                      {hit.description ? ` — ${hit.description}` : ""}
                    </div>
                  </div>
                  <button
                    type="button"
                    disabled={packages.includes(hit.attrName)}
                    onClick={() => addPackage(hit.attrName)}
                  >
                    {packages.includes(hit.attrName) ? "Added" : "Add"}
                  </button>
                </div>
              ))}
            </div>
            <div className="chips">
              {packages.map((attr) => (
                <span className="chip" key={attr}>
                  {attr}
                  <button type="button" onClick={() => removePackage(attr)}>
                    Remove
                  </button>
                </span>
              ))}
            </div>
          </section>
        </div>

        <div className="col">
          <section className="grow">
            <h2>Build</h2>
            <div className="row">
              <button
                type="button"
                className="primary"
                disabled={busy}
                onClick={() => void onBuild()}
              >
                Build image
              </button>
              {buildRunning ? (
                <button type="button" onClick={() => void onCancel()}>
                  Cancel
                </button>
              ) : null}
            </div>
            {status ? (
              <p className={statusOk ? "ok" : "error"}>{status}</p>
            ) : null}
            <LogPane lines={lines} />
          </section>

          <section>
            <h2>Flash</h2>
            <p className="hint">
              {image
                ? `Last image: ${image}`
                : "Build an image in this session before flashing."}
            </p>
            <div className="row">
              <button
                type="button"
                className="danger"
                disabled={!image || busy}
                onClick={() => {
                  setConfirmOpen(true);
                  void refreshDisks();
                }}
              >
                Flash
              </button>
            </div>
            {confirmOpen ? (
              <div className="confirm">
                <p>
                  This erases the selected disk. The raw image is written with
                  dd. Nothing is selected until you choose a device.
                </p>
                <div className="row">
                  <button
                    type="button"
                    disabled={loadingDisks || flashRunning}
                    onClick={() => void refreshDisks()}
                  >
                    {loadingDisks ? "Refreshing…" : "Refresh disks"}
                  </button>
                  <button
                    type="button"
                    disabled={flashRunning}
                    onClick={() => setConfirmOpen(false)}
                  >
                    Close
                  </button>
                </div>
                {diskError ? <p className="error">{diskError}</p> : null}
                {disks.length === 0 && !diskError ? (
                  <p className="hint">No removable or USB disks found.</p>
                ) : null}
                <div className="disks">
                  {disks.map((disk) => (
                    <label className="disk" key={disk.path}>
                      <input
                        type="radio"
                        name="disk"
                        checked={selected === disk.path}
                        onChange={() => setSelected(disk.path)}
                      />
                      <span>
                        <strong>{disk.path}</strong>
                        {" · "}
                        {disk.model ?? disk.name}
                        {" · "}
                        {formatBytes(disk.sizeBytes)}
                        {disk.tran ? ` · ${disk.tran}` : ""}
                        {disk.mountpoints.length > 0
                          ? ` · mounted ${disk.mountpoints.join(", ")}`
                          : ""}
                      </span>
                    </label>
                  ))}
                </div>
                <button
                  type="button"
                  className="danger"
                  disabled={!selected || busy}
                  onClick={() => {
                    if (selected) void onFlash(selected);
                  }}
                >
                  {selected ? `Erase ${selected}` : "Select a disk"}
                </button>
              </div>
            ) : null}
          </section>
        </div>
      </div>
    </div>
  );
}
