// installed by herdr
// managed by herdr; reinstalling or updating the integration overwrites this file.
// add custom hooks/plugins beside this file instead of editing it.
// HERDR_INTEGRATION_ID=opencode
// HERDR_INTEGRATION_VERSION=13

import net from "node:net";
import fs from "node:fs/promises";
import path from "node:path";

const SOURCE = "herdr:opencode";
const AGENT = "opencode";
let reportSeq = Date.now() * 1000;
let requestChain = Promise.resolve();
let reportedRootSessionID;
let usageWriteChain = Promise.resolve();
const reportedMessages = new Set();
const USAGE_LOG_LIMIT = 1024 * 1024;
const USAGE_RETENTION_MS = 8 * 24 * 60 * 60 * 1000;

function usageLogPath() {
  const stateRoot = process.env.XDG_STATE_HOME ||
    (process.platform === "win32"
      ? (process.env.LOCALAPPDATA || path.join(process.env.USERPROFILE || "", "AppData", "Local"))
      : path.join(process.env.HOME || "", ".local", "state"));
  return path.join(stateRoot, "herdr", "opencode-usage.jsonl");
}

function usageRecord(info) {
  const tokens = info?.tokens;
  const record = {
    ts: info?.time?.completed ?? info?.time?.created,
    providerID: info?.providerID,
    modelID: info?.modelID,
    cost: info?.cost,
    input: tokens?.input,
    output: tokens?.output,
  };
  if (!Number.isSafeInteger(record.ts) || record.ts <= 0 ||
      typeof record.providerID !== "string" || typeof record.modelID !== "string" ||
      !Number.isFinite(record.cost) || !Number.isFinite(record.input) || !Number.isFinite(record.output)) {
    return undefined;
  }
  return record;
}

function recordUsage(info) {
  const record = usageRecord(info);
  if (!record || reportedMessages.has(info.id)) return;
  reportedMessages.add(info.id);
  if (reportedMessages.size > 4096) reportedMessages.clear();
  const file = usageLogPath();
  const write = async () => {
    await fs.mkdir(path.dirname(file), { recursive: true });
    let lines = [];
    try {
      lines = (await fs.readFile(file, "utf8")).split("\n").filter((line) => {
        if (!line) return false;
        try { return JSON.parse(line).ts >= Date.now() - USAGE_RETENTION_MS; }
        catch { return false; }
      });
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    lines.push(JSON.stringify(record));
    let content = `${lines.join("\n")}\n`;
    while (Buffer.byteLength(content) > USAGE_LOG_LIMIT && lines.length > 1) {
      lines.shift();
      content = `${lines.join("\n")}\n`;
    }
    await fs.writeFile(file, content, { mode: 0o600 });
  };
  usageWriteChain = usageWriteChain.then(write).catch(() => {});
}

// Track child sessions so their events cannot replace the pane's root session.
// User prompts carry the root id to preserve its identity and cross-talk guard.
const childSessions = new Map();
const CHILD_EVENT_STATES = new Map([
  ["permission.asked", "blocked"],
  ["question.asked", "blocked"],
  ["permission.replied", "working"],
  ["question.replied", "working"],
  ["question.rejected", "working"],
]);

function nextReportSeq() {
  reportSeq += 1;
  return reportSeq;
}

function sessionIDFromProperties(properties) {
  return typeof properties?.sessionID === "string" && properties.sessionID
    ? properties.sessionID
    : undefined;
}

const SESSION_STATE_BY_STATUS = new Map([
  ["idle", "idle"],
  ["active", "working"],
  ["busy", "working"],
  ["pending", "working"],
  ["retry", "working"],
  ["running", "working"],
  ["streaming", "working"],
  ["working", "working"],
]);

function stateFromSessionStatus(status) {
  const kind = typeof status === "string" ? status : status?.type;
  return typeof kind === "string"
    ? SESSION_STATE_BY_STATUS.get(kind.toLowerCase())
    : undefined;
}

function request(method, params) {
  const pending = requestChain.then(() => requestOnce(method, params));
  requestChain = pending.catch(() => {});
  return pending;
}

function requestOnce(method, params) {
  const paneId = process.env.HERDR_PANE_ID;
  const socketPath = process.env.HERDR_SOCKET_PATH;

  if (!paneId || !socketPath) {
    return Promise.resolve();
  }

  const socketEndpoint =
    process.platform === "win32" ? `\\\\.\\pipe\\${socketPath}` : socketPath;

  const requestId = `${SOURCE}:${Date.now()}:${Math.floor(Math.random() * 1_000_000)
    .toString()
    .padStart(6, "0")}`;
  const request = {
    id: requestId,
    method,
    params: {
      pane_id: paneId,
      source: SOURCE,
      agent: AGENT,
      seq: nextReportSeq(),
      ...params,
    },
  };

  return new Promise((resolve) => {
    const client = net.createConnection(socketEndpoint, () => {
      client.write(`${JSON.stringify(request)}\n`);
    });

    const finish = () => {
      client.destroy();
      resolve();
    };

    client.setTimeout(500, finish);
    client.on("data", finish);
    client.on("error", finish);
    client.on("end", finish);
    client.on("close", resolve);
  });
}

function reportSession(sessionID) {
  if (!sessionID) {
    return Promise.resolve();
  }
  return request("pane.report_agent_session", { agent_session_id: sessionID });
}

function reportState(state, sessionID) {
  const params = { state };
  if (sessionID) {
    reportedRootSessionID = sessionID;
    params.agent_session_id = sessionID;
  }
  return request("pane.report_agent", params);
}

export const HerdrAgentStatePlugin = async () => {
  if (
    process.env.HERDR_ENV !== "1" ||
    !process.env.HERDR_SOCKET_PATH ||
    !process.env.HERDR_PANE_ID
  ) {
    return {};
  }

  return {
    "chat.message": async ({ sessionID }) => {
      if (sessionID && childSessions.has(sessionID)) {
        return;
      }
      await reportState("working", sessionID);
    },
    event: async ({ event }) => {
      const type = event?.type;
      const properties = event?.properties ?? {};
      const sessionID = sessionIDFromProperties(properties);

      const info = properties.info;
      if (type === "message.updated" && info?.role === "assistant" && info?.time?.completed) {
        recordUsage(info);
        await usageWriteChain;
      }
      if (info?.id && info.parentID) {
        childSessions.set(info.id, info.parentID);
      }
      if (sessionID && childSessions.has(sessionID)) {
        const state = CHILD_EVENT_STATES.get(type);
        if (state) {
          let rootSessionID = sessionID;
          while (childSessions.has(rootSessionID)) {
            rootSessionID = childSessions.get(rootSessionID);
          }
          await reportState(state, rootSessionID);
        }
        return;
      }

      switch (type) {
        case "session.created":
          // Creation is server-global, so an attached client may own it. The
          // TUI plugin separately reports the root selected in this pane.
          reportedRootSessionID = sessionID;
          break;
        case "session.updated":
          if (sessionID && sessionID !== reportedRootSessionID) {
            await reportSession(sessionID);
          }
          break;
        case "session.status": {
          const state = stateFromSessionStatus(properties.status);
          if (state) {
            await reportState(state, sessionID);
          } else {
            await reportSession(sessionID);
          }
          break;
        }
        case "tool.execute.before":
        case "tool.execute.after":
        case "permission.replied":
        case "question.replied":
        case "question.rejected":
        case "session.compacted":
          await reportState("working", sessionID);
          break;
        case "permission.asked":
        case "question.asked":
        case "session.error":
          await reportState("blocked", sessionID);
          break;
        case "session.idle":
          await reportState("idle", sessionID);
          break;
        case "session.deleted":
          break;
        default:
          break;
      }
    },
  };
};

// V1 (1.18.29+) calls server(). V2 calls setup() instead. Its shared server
// cannot attribute sessions using its process environment: the pane-local TUI
// owns both selection and lifecycle reporting there, including remote servers.
export default {
  id: "herdr.opencode",
  server: HerdrAgentStatePlugin,
  setup() {},
};
