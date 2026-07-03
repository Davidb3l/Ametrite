// Ametrite web app — vanilla TS, zero dependencies.

type Issue = {
  id: string; title: string; status: string; priority: string;
  project?: string; assignee?: string; parent?: string; due?: string;
  labels: string[]; claimed_by?: string; claim_expires_at?: string;
  created_at: string; updated_at: string; body?: string;
  activity?: Activity[]; backlinks?: DocRef[]; decisions?: Decision[];
};
type Decision = {
  id: string; title: string; resolves: string; status: string; superseded_by?: string;
};
type Activity = { seq: number; at: string; author: string; kind: string; body: string };
type DocRef = { id: string; type: string; title: string };

const STATUSES = ["backlog", "todo", "in_progress", "in_review", "done", "canceled"];
const STATUS_LABEL: Record<string, string> = {
  backlog: "Backlog", todo: "Todo", in_progress: "In Progress",
  in_review: "In Review", done: "Done", canceled: "Canceled",
};
const PRIORITIES = ["urgent", "high", "medium", "low", "none"];

const main = document.getElementById("main")!;
let projects: DocRef[] = [];
let boardFilter: { project?: string } = {};
let workspaces: any[] = [];
let currentWs = localStorage.getItem("amt-ws") ?? "";

// ---------- utilities ----------
function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));
}

function h(html: string): DocumentFragment {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content;
}

function toast(msg: string) {
  const el = document.createElement("div");
  el.className = "toast-msg";
  el.textContent = msg;
  document.getElementById("toast")!.appendChild(el);
  setTimeout(() => el.remove(), 4200);
}

async function api(path: string, init?: RequestInit): Promise<any> {
  if (currentWs) {
    path += (path.includes("?") ? "&" : "?") + "ws=" + encodeURIComponent(currentWs);
  }
  const res = await fetch(path, init);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    toast(data.error ?? `request failed (${res.status})`);
    throw new Error(data.error ?? String(res.status));
  }
  return data;
}

const post = (path: string, body: any) =>
  api(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
const patch = (path: string, body: any) =>
  api(path, { method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });

function ago(iso: string): string {
  const s = (Date.now() - Date.parse(iso)) / 1000;
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

function prioSvg(p: string): string {
  return `<svg class="prio p-${p}" viewBox="0 0 14 14" aria-label="${p}">
    <rect class="b1" x="1" y="8" width="3" height="5" rx="1"/>
    <rect class="b2" x="5.5" y="5" width="3" height="8" rx="1"/>
    <rect class="b3" x="10" y="2" width="3" height="11" rx="1"/>
  </svg>`;
}

function isStale(i: Issue): boolean {
  return !!i.claimed_by && !!i.claim_expires_at && Date.parse(i.claim_expires_at) < Date.now();
}

// ---------- tiny markdown renderer (escape first, then transform) ----------
function md(src: string): string {
  const lines = src.split("\n");
  const out: string[] = [];
  let inCode = false, inList = false;
  const closeList = () => { if (inList) { out.push("</ul>"); inList = false; } };
  const inline = (s: string): string =>
    esc(s)
      .replace(/\[\[([^\[\]]+)\]\]/g, (_, inner) => {
        const [target, alias] = inner.split("|");
        const clean = target.split("#")[0].trim();
        // [[ws:KEY]] — a link into another registered workspace's board.
        const xw = clean.match(/^([A-Za-z0-9][\w-]*):([A-Za-z0-9][\w-]*)$/);
        if (xw) {
          // `inner` is already HTML-escaped by esc(s) above — do NOT re-escape
          // the display text (the local-link branch below doesn't either).
          return `<a class="wikilink xws" href="#/x/${encodeURIComponent(xw[1])}/${encodeURIComponent(xw[2])}" title="${xw[1]} workspace">${alias?.trim() || clean}</a>`;
        }
        return `<a class="wikilink" href="#/doc/${encodeURIComponent(clean)}">${alias?.trim() || target.trim()}</a>`;
      })
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/(^|\s)\*([^*\s][^*]*)\*/g, "$1<em>$2</em>")
      .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
  for (const line of lines) {
    if (line.trim().startsWith("```")) {
      closeList();
      out.push(inCode ? "</code></pre>" : "<pre><code>");
      inCode = !inCode;
      continue;
    }
    if (inCode) { out.push(esc(line) + "\n"); continue; }
    const hm = line.match(/^(#{1,3})\s+(.*)/);
    if (hm) { closeList(); out.push(`<h${hm[1].length}>${inline(hm[2])}</h${hm[1].length}>`); continue; }
    if (/^\s*[-*]\s+/.test(line)) {
      if (!inList) { out.push("<ul>"); inList = true; }
      out.push(`<li>${inline(line.replace(/^\s*[-*]\s+/, ""))}</li>`);
      continue;
    }
    closeList();
    if (line.startsWith(">")) { out.push(`<blockquote>${inline(line.slice(1).trim())}</blockquote>`); continue; }
    if (line.trim() === "") continue;
    out.push(`<p>${inline(line)}</p>`);
  }
  closeList();
  if (inCode) out.push("</code></pre>");
  return out.join("\n");
}

// ---------- routing ----------
function route(): { view: string; arg?: string } {
  const hash = location.hash.replace(/^#\/?/, "");
  const [view, ...rest] = hash.split("/");
  return { view: view || "board", arg: rest.join("/") ? decodeURIComponent(rest.join("/")) : undefined };
}

async function render() {
  const r = route();
  document.querySelectorAll("nav a").forEach((a) =>
    a.classList.toggle("active", (a as HTMLAnchorElement).dataset.nav === r.view));
  try {
    if (r.view === "board") await renderBoard();
    else if (r.view === "inbox") await renderInbox();
    else if (r.view === "issue" && r.arg) await renderIssue(r.arg);
    else if (r.view === "notes") await renderNotes(r.arg);
    else if (r.view === "search") renderSearch();
    else if (r.view === "doc" && r.arg) await renderDocRedirect(r.arg);
    else if (r.view === "x" && r.arg) await crossWorkspace(r.arg);
    else location.hash = "#/board";
  } catch (e) {
    // toast already shown by api()
  }
}

// [[ws:KEY]] follow: switch to workspace `alias`, then open `KEY`. If the
// alias isn't registered here, fall back to the Inbox rather than 404.
async function crossWorkspace(arg: string) {
  const [alias, ...rest] = arg.split("/");
  const key = rest.join("/");
  const target = workspaces.find((w) => w.alias === alias);
  if (!target) { toast(`workspace “${alias}” not registered`); location.hash = "#/inbox"; return; }
  if (alias !== currentWs) {
    currentWs = alias;
    localStorage.setItem("amt-ws", currentWs);
    await loadSidebar();
  }
  location.hash = key ? `#/doc/${encodeURIComponent(key)}` : "#/board";
}

async function renderDocRedirect(id: string) {
  const doc = await api(`/api/docs/${encodeURIComponent(id)}`).catch(() => null);
  if (!doc) { main.innerHTML = `<div class="content"><div class="empty">No document “${esc(id)}” — it may not exist yet.</div></div>`; return; }
  if (doc.type === "issue") location.hash = `#/issue/${doc.id}`;
  else location.hash = `#/notes/${doc.id}`;
}

// ---------- board ----------
async function renderBoard() {
  const params = new URLSearchParams({ all: "1" });
  if (boardFilter.project) params.set("project", boardFilter.project);
  const issues: Issue[] = await api(`/api/issues?${params}`);
  const byStatus: Record<string, Issue[]> = {};
  for (const s of STATUSES) byStatus[s] = [];
  for (const i of issues) (byStatus[i.status] ??= []).push(i);

  main.innerHTML = "";
  main.append(h(`
    <div class="topbar">
      <h1>Board${boardFilter.project ? ` · ${esc(boardFilter.project)}` : ""}</h1>
      ${boardFilter.project ? '<a class="crumb" href="#/board" id="clear-filter">clear filter ✕</a>' : ""}
      <div class="spacer"></div>
      <button class="primary" id="new-issue">＋ New issue</button>
    </div>
    <div class="content"><div class="board" id="board"></div></div>
  `));
  main.querySelector("#clear-filter")?.addEventListener("click", () => { boardFilter = {}; });
  main.querySelector("#new-issue")!.addEventListener("click", () => issueDialog());

  const board = main.querySelector("#board")!;
  for (const status of STATUSES) {
    const col = document.createElement("div");
    col.className = "column";
    col.innerHTML = `
      <div class="col-head">
        <span class="st-dot" style="background:var(--st-${status})"></span>
        ${STATUS_LABEL[status]}
        <span class="count">${byStatus[status].length}</span>
        <button class="add" title="New issue in ${STATUS_LABEL[status]}">＋</button>
      </div>
      <div class="col-body" data-status="${status}"></div>`;
    col.querySelector(".add")!.addEventListener("click", () => issueDialog(status));
    const body = col.querySelector(".col-body")! as HTMLElement;
    for (const issue of byStatus[status]) body.appendChild(card(issue));
    body.addEventListener("dragover", (e) => { e.preventDefault(); body.classList.add("drop-target"); });
    body.addEventListener("dragleave", () => body.classList.remove("drop-target"));
    body.addEventListener("drop", async (e) => {
      e.preventDefault();
      body.classList.remove("drop-target");
      const id = e.dataTransfer!.getData("text/amt-issue");
      if (!id) return;
      await patch(`/api/issues/${id}`, { status });
      render();
    });
    board.appendChild(col);
  }
}

// ---------- inbox (cross-workspace) ----------
async function renderInbox() {
  const issues: (Issue & { workspace: string; workspace_name: string })[] = await api("/api/inbox");
  main.innerHTML = "";
  main.append(h(`
    <div class="topbar">
      <h1>Inbox</h1>
      <span class="crumb">${issues.length} open across ${new Set(issues.map((i) => i.workspace)).size} workspace(s)</span>
      <div class="spacer"></div>
    </div>
    <div class="content"><div class="inbox" id="inbox"></div></div>
  `));
  const box = main.querySelector("#inbox")!;
  if (!issues.length) {
    box.innerHTML = '<div class="empty big"><span class="facet"></span>Nothing open anywhere. Enjoy it.</div>';
    return;
  }
  let lastPrio = "";
  for (const i of issues) {
    if (i.priority !== lastPrio) {
      lastPrio = i.priority;
      box.append(h(`<div class="inbox-group">${prioSvg(i.priority)}<span>${esc(i.priority)}</span></div>`));
    }
    const claim = i.claimed_by
      ? `<span class="claim ${isStale(i) ? "stale" : ""}">🔒 ${esc(i.claimed_by)}</span>` : "";
    const row = h(`
      <a class="inbox-row" href="#/x/${encodeURIComponent(i.workspace)}/${encodeURIComponent(i.id)}">
        <span class="ws-badge" title="${esc(i.workspace_name)}">${esc(i.workspace_name)}</span>
        <span class="key">${esc(i.id)}</span>
        <span class="ititle">${esc(i.title)}</span>
        <span class="st-dot" style="background:var(--st-${i.status})" title="${esc(i.status)}"></span>
        ${i.labels.slice(0, 2).map((l) => `<span class="chip">${esc(l)}</span>`).join("")}
        ${claim}
      </a>`);
    box.append(row);
  }
}

function card(i: Issue): HTMLElement {
  const el = document.createElement("div");
  el.className = "card";
  el.draggable = true;
  const claim = i.claimed_by
    ? `<span class="claim ${isStale(i) ? "stale" : ""}" title="${isStale(i) ? "stale lease — claimable" : `lease until ${i.claim_expires_at}`}">🔒 ${esc(i.claimed_by)}</span>`
    : "";
  el.innerHTML = `
    <span class="key">${i.id}</span>
    <a class="title" href="#/issue/${i.id}">${esc(i.title)}</a>
    <div class="meta">
      ${prioSvg(i.priority)}
      ${i.labels.slice(0, 3).map((l) => `<span class="chip">${esc(l)}</span>`).join("")}
      ${claim}
    </div>`;
  el.addEventListener("dragstart", (e) => {
    e.dataTransfer!.setData("text/amt-issue", i.id);
    el.classList.add("dragging");
  });
  el.addEventListener("dragend", () => el.classList.remove("dragging"));
  return el;
}

function issueDialog(status?: string) {
  const dlg = document.createElement("dialog");
  dlg.innerHTML = `
    <div class="dlg-head">New issue</div>
    <form method="dialog">
      <input name="title" placeholder="Issue title" required autofocus />
      <div class="row2">
        <select name="priority">
          ${PRIORITIES.map((p) => `<option value="${p}" ${p === "none" ? "selected" : ""}>${p}</option>`).join("")}
        </select>
        <select name="project">
          <option value="">no project</option>
          ${projects.map((p) => `<option value="${esc(p.id)}">${esc(p.title)}</option>`).join("")}
        </select>
      </div>
      <input name="labels" placeholder="labels, comma separated" />
      <textarea name="body" placeholder="Description — markdown, [[wikilinks]] and #tags supported"></textarea>
      <div class="dlg-actions">
        <button type="button" class="cancel">Cancel</button>
        <button class="primary" type="submit">Create</button>
      </div>
    </form>`;
  document.body.appendChild(dlg);
  dlg.querySelector(".cancel")!.addEventListener("click", () => dlg.close());
  dlg.addEventListener("close", () => dlg.remove());
  dlg.querySelector("form")!.addEventListener("submit", async (e) => {
    const f = new FormData(e.target as HTMLFormElement);
    const created: Issue = await post("/api/issues", {
      title: f.get("title"),
      priority: f.get("priority"),
      project: f.get("project") || undefined,
      body: f.get("body") || undefined,
      labels: String(f.get("labels") ?? "").split(",").map((s) => s.trim()).filter(Boolean),
    });
    if (status && status !== "backlog") await patch(`/api/issues/${created.id}`, { status });
    render();
  });
  dlg.showModal();
}

// ---------- issue detail ----------
async function renderIssue(id: string) {
  const i: Issue = await api(`/api/issues/${encodeURIComponent(id)}`);
  main.innerHTML = "";
  main.append(h(`
    <div class="topbar">
      <a class="crumb" href="#/board">← Board</a>
      <h1>${i.id}</h1>
      <div class="spacer"></div>
    </div>
    <div class="content">
      <div class="issue-page">
        <div class="issue-main">
          <div class="issue-key">${i.id}${i.parent ? ` · sub-issue of <a class="wikilink" href="#/issue/${esc(i.parent)}">${esc(i.parent)}</a>` : ""}</div>
          <h2 class="issue-title">${esc(i.title)}</h2>
          <div class="prose">${i.body?.trim() ? md(i.body) : '<p style="color:var(--text-3)">No description.</p>'}</div>
          <div class="activity">
            <div class="side-heading">Activity</div>
            <div id="activity"></div>
            <div class="comment-box">
              <textarea id="comment" placeholder="Leave a comment…"></textarea>
              <div class="row"><button class="primary" id="send-comment">Comment</button></div>
            </div>
          </div>
        </div>
        <div class="side-panel">
          <div class="panel-block" id="fields"></div>
          <div class="panel-block">
            <div class="side-heading">Decisions</div>
            <div id="decisions"></div>
            <button id="new-decision" class="ghost-add">＋ Record decision</button>
          </div>
          <div class="panel-block">
            <div class="side-heading">Backlinks</div>
            <div id="backlinks"></div>
          </div>
        </div>
      </div>
    </div>
  `));

  const fields = main.querySelector("#fields")!;
  const fieldRow = (label: string, control: string) =>
    `<div class="field-row"><span class="lbl">${label}</span>${control}</div>`;
  fields.innerHTML =
    fieldRow("Status", `<select id="f-status">${STATUSES.map((s) =>
      `<option value="${s}" ${s === i.status ? "selected" : ""}>${STATUS_LABEL[s]}</option>`).join("")}</select>`) +
    fieldRow("Priority", `<select id="f-priority">${PRIORITIES.map((p) =>
      `<option ${p === i.priority ? "selected" : ""}>${p}</option>`).join("")}</select>`) +
    fieldRow("Assignee", `<span class="val">${esc(i.assignee ?? "—")}</span>`) +
    fieldRow("Project", `<span class="val">${i.project ? `<a class="wikilink" href="#/doc/${esc(i.project)}">${esc(i.project)}</a>` : "—"}</span>`) +
    (i.due ? fieldRow("Due", `<span class="val">${i.due}</span>`) : "") +
    fieldRow("Labels", `<span class="val">${i.labels.map(esc).join(", ") || "—"}</span>`) +
    (i.claimed_by
      ? fieldRow("Claim", `<span class="claim ${isStale(i) ? "stale" : ""}">🔒 ${esc(i.claimed_by)}</span>`)
      : "");
  fields.querySelector("#f-status")!.addEventListener("change", async (e) => {
    await patch(`/api/issues/${i.id}`, { status: (e.target as HTMLSelectElement).value });
    render();
  });
  fields.querySelector("#f-priority")!.addEventListener("change", async (e) => {
    await patch(`/api/issues/${i.id}`, { priority: (e.target as HTMLSelectElement).value });
    render();
  });

  const decisions = main.querySelector("#decisions")!;
  decisions.innerHTML = i.decisions?.length
    ? i.decisions.map((d) => `
        <div class="decision-item ${d.status}">
          <a href="#/doc/${encodeURIComponent(d.id)}"><span class="key">${esc(d.id)}</span> ${esc(d.title)}</a>
          <span class="d-status">${d.status === "superseded" ? `superseded by ${esc(d.superseded_by ?? "")}` : d.status}</span>
        </div>`).join("")
    : '<div class="empty" style="padding:4px 0;text-align:left">No decisions recorded.</div>';
  main.querySelector("#new-decision")!.addEventListener("click", () => decisionDialog(i));

  const bl = main.querySelector("#backlinks")!;
  bl.innerHTML = i.backlinks?.length
    ? i.backlinks.map((b) =>
        `<div class="backlink-item"><span class="key">${esc(b.id)}</span><a href="#/doc/${encodeURIComponent(b.id)}">${esc(b.title)}</a></div>`).join("")
    : '<div class="empty" style="padding:6px 0">Nothing links here yet.</div>';

  const act = main.querySelector("#activity")!;
  act.innerHTML = (i.activity ?? []).map((a) =>
    a.kind === "comment"
      ? `<div class="activity-entry comment"><div><span class="who">@${esc(a.author)}</span>
           <span class="when">${ago(a.at)} ago</span><div class="prose">${md(a.body)}</div></div></div>`
      : `<div class="activity-entry event"><span class="when">${ago(a.at)}</span>
           <span class="what"><span class="who">@${esc(a.author)}</span> ${esc(a.body)}</span></div>`
  ).join("") || '<div class="empty" style="padding:6px 0">No activity.</div>';

  main.querySelector("#send-comment")!.addEventListener("click", async () => {
    const ta = main.querySelector("#comment") as HTMLTextAreaElement;
    if (!ta.value.trim()) return;
    await post(`/api/issues/${i.id}/comments`, { body: ta.value });
    render();
  });
}

function decisionDialog(issue: Issue) {
  const active = (issue.decisions ?? []).filter((d) => d.status !== "superseded");
  const dlg = document.createElement("dialog");
  dlg.innerHTML = `
    <div class="dlg-head">Record decision · ${esc(issue.id)}</div>
    <form method="dialog">
      <input name="title" placeholder="Decision — e.g. 'Use SQLite as source of truth'" required autofocus />
      <textarea name="body" placeholder="## Context&#10;…&#10;## Decision&#10;…&#10;## Consequences&#10;…"></textarea>
      <div class="row2">
        <select name="status">
          <option value="accepted" selected>accepted</option>
          <option value="proposed">proposed</option>
        </select>
        <select name="supersedes">
          <option value="">supersedes nothing</option>
          ${active.map((d) => `<option value="${esc(d.id)}">supersedes ${esc(d.id)} — ${esc(d.title)}</option>`).join("")}
        </select>
      </div>
      <div class="dlg-actions">
        <button type="button" class="cancel">Cancel</button>
        <button class="primary" type="submit">Record</button>
      </div>
    </form>`;
  document.body.appendChild(dlg);
  dlg.querySelector(".cancel")!.addEventListener("click", () => dlg.close());
  dlg.addEventListener("close", () => dlg.remove());
  dlg.querySelector("form")!.addEventListener("submit", async (e) => {
    const f = new FormData(e.target as HTMLFormElement);
    await post("/api/decisions", {
      issue: issue.id,
      title: f.get("title"),
      body: f.get("body") || undefined,
      status: f.get("status"),
      supersedes: f.get("supersedes") || undefined,
    });
    render();
  });
  dlg.showModal();
}

// ---------- notes ----------
async function renderNotes(selected?: string) {
  const notes: any[] = await api("/api/notes");
  main.innerHTML = "";
  main.append(h(`
    <div class="topbar">
      <h1>Notes</h1>
      <div class="spacer"></div>
      <button class="primary" id="new-note">＋ New note</button>
    </div>
    <div class="content">
      <div class="notes-page">
        <div class="notes-list" id="note-list"></div>
        <div class="note-view" id="note-view"><div class="empty big"><span class="facet"></span>Select a note — or create one.</div></div>
      </div>
    </div>
  `));
  const list = main.querySelector("#note-list")!;
  list.innerHTML = notes.length
    ? notes.map((n) =>
        `<a class="note-item ${n.id === selected ? "active" : ""}" href="#/notes/${encodeURIComponent(n.id)}">
           <span class="t">${esc(n.title)}</span><span class="d">${ago(n.updated_at)} ago</span></a>`).join("")
    : '<div class="empty">No notes yet.</div>';

  main.querySelector("#new-note")!.addEventListener("click", () => {
    const dlg = document.createElement("dialog");
    dlg.innerHTML = `
      <div class="dlg-head">New note</div>
      <form method="dialog">
        <input name="title" placeholder="Note title" required autofocus />
        <input name="tags" placeholder="tags, comma separated" />
        <textarea name="body" placeholder="Markdown — link work with [[AMT-1]], link ideas with [[Other Note]]"></textarea>
        <div class="dlg-actions">
          <button type="button" class="cancel">Cancel</button>
          <button class="primary" type="submit">Create</button>
        </div>
      </form>`;
    document.body.appendChild(dlg);
    dlg.querySelector(".cancel")!.addEventListener("click", () => dlg.close());
    dlg.addEventListener("close", () => dlg.remove());
    dlg.querySelector("form")!.addEventListener("submit", async (e) => {
      const f = new FormData(e.target as HTMLFormElement);
      const doc = await post("/api/notes", {
        title: f.get("title"),
        body: f.get("body") || undefined,
        tags: String(f.get("tags") ?? "").split(",").map((s) => s.trim()).filter(Boolean),
      });
      location.hash = `#/notes/${encodeURIComponent(doc.id)}`;
    });
    dlg.showModal();
  });

  if (selected) {
    const doc = await api(`/api/docs/${encodeURIComponent(selected)}`).catch(() => null);
    if (doc) {
      const view = main.querySelector("#note-view")!;
      view.innerHTML = `
        <div class="issue-key">${esc(doc.id)}</div>
        <h2 class="issue-title">${esc(doc.title)}</h2>
        <div class="tags">${doc.tags.map((t: string) => `<span class="chip">#${esc(t)}</span>`).join("")}</div>
        <div class="prose">${doc.body?.trim() ? md(doc.body) : '<p style="color:var(--text-3)">Empty note.</p>'}</div>
        <div class="activity">
          <div class="side-heading">Backlinks</div>
          ${doc.backlinks?.length
            ? doc.backlinks.map((b: DocRef) =>
                `<div class="backlink-item"><span class="key">${esc(b.id)}</span><a href="#/doc/${encodeURIComponent(b.id)}">${esc(b.title)}</a></div>`).join("")
            : '<div class="empty" style="padding:6px 0;text-align:left">Nothing links here yet.</div>'}
        </div>`;
    }
  }
}

// ---------- search ----------
function renderSearch() {
  main.innerHTML = "";
  main.append(h(`
    <div class="topbar"><h1>Search</h1><div class="spacer"></div></div>
    <div class="content"><div class="search-page">
      <div class="bar"><input type="search" id="q" placeholder="Search issues, notes, projects… (FTS5, no embeddings)" autofocus /></div>
      <div class="filter-chips" id="chips">
        <span class="chip on" data-type="">all</span>
        <span class="chip" data-type="issue">issues</span>
        <span class="chip" data-type="note">notes</span>
        <span class="chip" data-type="project">projects</span>
      </div>
      <div id="results"></div>
    </div></div>
  `));
  const q = main.querySelector("#q") as HTMLInputElement;
  const results = main.querySelector("#results")!;
  let type = "";
  let timer: any;
  const run = async () => {
    if (!q.value.trim()) { results.innerHTML = ""; return; }
    const params = new URLSearchParams({ q: q.value });
    if (type) params.set("type", type);
    const hits: any[] = await api(`/api/search?${params}`);
    results.innerHTML = hits.length
      ? hits.map((hit) => `
          <a class="hit" href="#/doc/${encodeURIComponent(hit.id)}">
            <div class="h-top"><span class="h-key">${esc(hit.id)}</span>
              <span class="h-title">${esc(hit.title)}</span>
              <span class="h-type">${hit.type}</span></div>
            ${hit.snippet ? `<div class="h-snip">${esc(hit.snippet)}</div>` : ""}
          </a>`).join("")
      : '<div class="empty big"><span class="facet"></span>No results.</div>';
  };
  q.addEventListener("input", () => { clearTimeout(timer); timer = setTimeout(run, 180); });
  main.querySelector("#chips")!.addEventListener("click", (e) => {
    const chip = (e.target as HTMLElement).closest(".chip") as HTMLElement | null;
    if (!chip) return;
    main.querySelectorAll("#chips .chip").forEach((c) => c.classList.remove("on"));
    chip.classList.add("on");
    type = chip.dataset.type ?? "";
    run();
  });
}

// ---------- sidebar data + live updates ----------
async function loadSidebar() {
  workspaces = await api("/api/workspaces").catch(() => []);
  if (workspaces.length && !workspaces.some((w) => w.alias === currentWs)) {
    currentWs = workspaces[0].alias;
    localStorage.setItem("amt-ws", currentWs);
  }
  const wsList = document.getElementById("ws-list")!;
  wsList.innerHTML = workspaces.map((w) => `
    <a class="project-link ws-link ${w.alias === currentWs ? "active" : ""}" href="#/board" data-ws="${esc(w.alias)}">
      <span class="dot"></span>${esc(w.name)}<span class="ws-count">${w.open_issues}</span>
    </a>`).join("");
  wsList.querySelectorAll(".ws-link").forEach((el) =>
    el.addEventListener("click", () => {
      currentWs = (el as HTMLElement).dataset.ws!;
      localStorage.setItem("amt-ws", currentWs);
      boardFilter = {};
      loadSidebar().then(render);
    }));
  const ws = await api("/api/workspace").catch(() => null);
  if (ws) document.getElementById("ws-name")!.textContent = ws.name;
  projects = await api("/api/projects").catch(() => []);
  const list = document.getElementById("project-list")!;
  list.innerHTML = projects.map((p) =>
    `<a class="project-link" href="#/board" data-project="${esc(p.id)}"><span class="dot"></span>${esc(p.title)}</a>`).join("");
  list.querySelectorAll(".project-link").forEach((a) =>
    a.addEventListener("click", () => {
      boardFilter = { project: (a as HTMLElement).dataset.project };
    }));
}

function connectSSE() {
  const es = new EventSource("/api/events");
  const dot = document.getElementById("live-dot")!;
  es.addEventListener("hello", () => dot.classList.add("on"));
  let timer: any;
  es.addEventListener("change", (e) => {
    const ws = JSON.parse((e as MessageEvent).data ?? "{}").ws;
    clearTimeout(timer);
    timer = setTimeout(() => {
      loadSidebar();
      if (!ws || ws === currentWs) render();
    }, 150);
  });
  // A workspace was registered live (e.g. `amt init` elsewhere) — refresh the
  // sidebar, and re-render the Inbox since it spans every workspace (AMT-10).
  es.addEventListener("workspaces", () => {
    loadSidebar().then(() => { if (route().view === "inbox") render(); });
  });
  es.onerror = () => {
    dot.classList.remove("on");
    es.close();
    setTimeout(connectSSE, 3000);
  };
}

// ---------- keyboard shortcuts ----------
window.addEventListener("keydown", (e) => {
  const t = e.target as HTMLElement;
  if (
    t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement || t.closest?.("dialog") ||
    e.metaKey || e.ctrlKey || e.altKey
  ) return;
  if (e.key === "b") location.hash = "#/board";
  else if (e.key === "i") location.hash = "#/inbox";
  else if (e.key === "n") location.hash = "#/notes";
  else if (e.key === "/") { location.hash = "#/search"; e.preventDefault(); }
  else if (e.key === "c" && route().view === "board") { issueDialog(); e.preventDefault(); }
});

window.addEventListener("hashchange", render);
loadSidebar().then(render);
connectSSE();
