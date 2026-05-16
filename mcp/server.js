#!/usr/bin/env node
/**
 * PreXiv MCP server
 *
 * Exposes the PreXiv REST API as Model Context Protocol tools so MCP-aware
 * agents (Claude Desktop, the Anthropic Agent SDK, any MCP client) can search,
 * read, submit, and discuss manuscripts on a running PreXiv instance.
 *
 * Transports:
 *   - stdio (default) — what Claude Desktop launches as a subprocess.
 *   - HTTP (opt-in)    — set MCP_TRANSPORT=http; Streamable HTTP on MCP_PORT
 *                        (default 3100). Useful for "remote" agent setups.
 *
 * Configuration (env vars):
 *   PREXIV_API_URL  base URL of the running PreXiv API.  default http://localhost:3000/api/v1
 *   PREXIV_TOKEN    optional bearer token (`prexiv_<secret>`).  Required for write tools.
 *   MCP_TRANSPORT   `stdio` (default) or `http`.
 *   MCP_PORT        port for HTTP transport. default 3100.
 *   MCP_HOST        bind interface for HTTP transport. default 127.0.0.1.
 *   MCP_HTTP_TOKEN  optional auth secret for HTTP transport. Required when
 *                   binding HTTP transport to a non-loopback interface.
 *
 * The server has zero dependencies beyond `@modelcontextprotocol/sdk` (and its
 * peer `zod`); HTTP is performed via Node's built-in `fetch`.
 */

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from '@modelcontextprotocol/sdk/types.js';

// -----------------------------------------------------------------------------
// Configuration
// -----------------------------------------------------------------------------

function configError(message) {
  // eslint-disable-next-line no-console
  console.error(`prexiv-mcp config error: ${message}`);
  process.exit(2);
}

function normalizeApiUrl(raw) {
  let u;
  try {
    u = new URL(raw);
  } catch {
    configError(`PREXIV_API_URL must be an absolute URL ending in /api/v1; got ${JSON.stringify(raw)}`);
  }
  u.hash = '';
  u.search = '';
  u.pathname = u.pathname.replace(/\/+$/, '');
  if (!u.pathname.endsWith('/api/v1')) {
    configError(`PREXIV_API_URL must point at the PreXiv JSON API and end in /api/v1; got ${u.toString()}`);
  }
  return u.toString().replace(/\/+$/, '');
}

const API_URL = normalizeApiUrl(process.env.PREXIV_API_URL || 'http://localhost:3000/api/v1');
const TOKEN = process.env.PREXIV_TOKEN || '';
const TRANSPORT = (process.env.MCP_TRANSPORT || 'stdio').toLowerCase();
const PORT = Number(process.env.MCP_PORT || 3100);
const HOST = process.env.MCP_HOST || '127.0.0.1';
const HTTP_AUTH_TOKEN = process.env.MCP_HTTP_TOKEN || '';

// Roles enum, reused by several tool schemas.
const ROLES = [
  'undergraduate',
  'graduate-student',
  'postdoc',
  'industry-researcher',
  'professor',
  'professional-expert',
  'independent-researcher',
  'hobbyist',
];

// -----------------------------------------------------------------------------
// REST API client
// -----------------------------------------------------------------------------

/**
 * Thin wrapper around `fetch` that targets the PreXiv REST API.
 *
 * @param {string} method  HTTP verb (GET, POST, DELETE).
 * @param {string} path    Path under the configured base URL, e.g. `/manuscripts`.
 *                         Leading slash is required; query string allowed.
 * @param {object} [body]  JSON-serializable request body (POST only).
 * @param {object} [opts]  { requireAuth?: boolean }.  When true and no token is
 *                         set, throws a friendly error before contacting the API.
 * @returns {Promise<any>} Parsed JSON response (or `null` on 204 No Content).
 *
 * Errors are thrown as `Error` instances whose `.message` is human-readable;
 * the underlying response body (when JSON) is attached at `.cause` for
 * diagnostics.
 */
async function apiCall(method, path, body, opts = {}) {
  if (opts.requireAuth && !TOKEN) {
    throw new Error(
      'PREXIV_TOKEN is not set. This tool writes to PreXiv and requires a bearer token.\n' +
      'Get one from /me/tokens after verifying the account through GitHub OAuth, ORCID OAuth, or email.\n' +
      'Then export PREXIV_TOKEN=prexiv_xxx... before launching the MCP server.',
    );
  }
  const url = API_URL + path;
  const headers = { 'Accept': 'application/json' };
  if (TOKEN) headers['Authorization'] = `Bearer ${TOKEN}`;
  const init = { method, headers };
  if (body !== undefined && body !== null) {
    headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  let res;
  try {
    res = await fetch(url, init);
  } catch (e) {
    throw new Error(
      `network error contacting PreXiv API at ${url}: ${e?.message || e}.\n` +
      `Is the PreXiv server running and reachable at PREXIV_API_URL?`,
    );
  }
  // 204 No Content (e.g., DELETE /comments/:id)
  if (res.status === 204) return null;
  const contentType = res.headers.get('content-type') || '';
  let data = null;
  if (contentType.includes('application/json')) {
    try {
      data = await res.json();
    } catch {
      data = null;
    }
  } else {
    // Non-JSON responses mean the configured base is not the PreXiv JSON API
    // (or a proxy returned an HTML error page). Do not hand arbitrary text
    // back to an agent as if it were trusted API output.
    const text = await res.text().catch(() => '');
    throw new Error(
      `PreXiv API ${method} ${path} returned non-JSON HTTP ${res.status} ${res.statusText}` +
        (text ? ` — ${text.slice(0, 300)}` : ''),
    );
  }
  if (!res.ok) {
    const detail = data && data.error
      ? data.error +
        (Array.isArray(data.details) && data.details.length
          ? ` (${data.details.map((d) => (typeof d === 'string' ? d : JSON.stringify(d))).join('; ')})`
          : '')
      : `HTTP ${res.status} ${res.statusText}`;
    const err = new Error(`PreXiv API ${method} ${path} failed: ${detail}`);
    err.cause = data;
    throw err;
  }
  return data;
}

// -----------------------------------------------------------------------------
// Tool definitions
// -----------------------------------------------------------------------------
//
// Each entry is:
//   { name, description, inputSchema (JSON Schema), handler, requireAuth? }
//
// Handlers take the validated arguments object and return a JSON-serializable
// result that we wrap in MCP `content` blocks below.

/** Build the JSON-Schema fragment for `prexiv_submit`. */
function manuscriptFieldsSchema({ allRequired }) {
  const props = {
    title: { type: 'string', minLength: 5, maxLength: 300, description: 'Manuscript title (5–300 chars).' },
    abstract: { type: 'string', minLength: 100, maxLength: 5000, description: 'Abstract (100–5000 chars). Markdown + LaTeX `$..$` / `$$..$$` allowed.' },
    authors: { type: 'string', description: 'Authors or responsible credit line as a single semicolon-separated string, e.g. `Jane Doe; Example Lab`. Disclose AI tools in conductor_ai_models, not as legal authors.' },
    category: { type: 'string', description: 'Category id from /categories, e.g. `cs.LG`, `hep-th`, `math.NT`.' },
    external_url: { type: 'string', format: 'uri', description: 'Optional supplemental external URL. The primary artifact must still be uploaded with source_base64 or pdf_base64.' },
    source_base64: { type: 'string', description: 'Base64-encoded .tex, .zip, .tar.gz, or .tgz source. Mutually exclusive with pdf_base64. Required for redaction of private conductor/model fields.' },
    source_filename: { type: 'string', description: 'Filename for source_base64, e.g. `main.tex` or `paper.zip`.' },
    pdf_base64: { type: 'string', description: 'Base64-encoded finished PDF. Mutually exclusive with source_base64. Cannot be used when private conductor/model fields require redaction.' },
    pdf_filename: { type: 'string', description: 'Filename for pdf_base64, e.g. `paper.pdf`.' },
    conductor_type: { type: 'string', enum: ['human-ai', 'ai-agent'], description: '`human-ai` = a named human directed an AI; `ai-agent` = autonomous AI agent.' },
    conductor_ai_model: { type: 'string', description: 'Legacy single-model shape. Comma-separated string acceptable: `Claude Opus 4.7, GPT-5.5 Pro`. Prefer `conductor_ai_models` (array) for new submissions.' },
    conductor_ai_models: { type: 'array', items: { type: 'string' }, description: 'Preferred shape for multi-model AI provenance. Each entry is one precise model+version string, e.g. `["Claude Opus 4.7", "GPT-5.5 Pro", "Gemini 3 Pro"]`. List every model that actually contributed.' },
    conductor_ai_model_public: { type: 'boolean', description: 'Default true. False hides all AI model names from the public manuscript page and requires source_base64 so PreXiv can produce redacted public source/PDF.' },
    conductor_human: { type: 'string', description: 'Required when conductor_type=human-ai. Display name of the human conductor.' },
    conductor_human_public: { type: 'boolean', description: 'Default true. False hides the human conductor name and requires source_base64 so PreXiv can produce redacted public source/PDF.' },
    conductor_role: { type: 'string', enum: ROLES, description: 'Optional role for human-ai submissions.' },
    conductor_notes: { type: 'string', description: 'Optional free-form notes about the conductor or the production process.' },
    agent_framework: { type: 'string', description: 'Optional. For conductor_type=ai-agent, the framework or harness used (e.g. `Anthropic Agent SDK`).' },
    has_auditor: { type: 'boolean', description: 'Whether a named human auditor has signed a scoped correctness statement.' },
    auditor_name: { type: 'string', description: 'Required when has_auditor=true. The auditor\'s display name.' },
    auditor_affiliation: { type: 'string', description: 'Optional auditor affiliation.' },
    auditor_role: { type: 'string', enum: ROLES, description: 'Required when has_auditor=true. The auditor\'s role.' },
    auditor_statement: { type: 'string', minLength: 20, description: 'Required when has_auditor=true. Signed, scoped correctness statement (≥20 chars).' },
    auditor_orcid: { type: 'string', description: 'Optional auditor ORCID iD.' },
    license: { type: 'string', description: 'Reader license id, e.g. `CC-BY-4.0`. Defaults to CC-BY-4.0.' },
    ai_training: { type: 'string', description: 'AI-training policy: `allow`, `allow-with-attribution`, or `disallow`. Defaults to allow.' },
  };
  // `conductor_ai_model` and `conductor_ai_models` are alternatives.
  // The server-side validator accepts either, so we mark neither as
  // strictly required in JSON Schema; downstream `required` collapses
  // them via a custom check rather than a schema-level requirement.
  const required = allRequired
    ? ['title', 'abstract', 'authors', 'category', 'conductor_type']
    : [];
  return {
    type: 'object',
    properties: props,
    required,
    ...(allRequired
      ? {
          oneOf: [
            { required: ['source_base64', 'source_filename'] },
            { required: ['pdf_base64', 'pdf_filename'] },
          ],
        }
      : {}),
    additionalProperties: false,
  };
}

function revisionFieldsSchema() {
  return {
    type: 'object',
    properties: {
      title: { type: 'string', minLength: 5, maxLength: 300, description: 'Replacement title.' },
      abstract: { type: 'string', minLength: 100, maxLength: 5000, description: 'Replacement abstract.' },
      authors: { type: 'string', description: 'Replacement author/responsible-credit line.' },
      category: { type: 'string', description: 'Replacement category id.' },
      external_url: { type: 'string', format: 'uri', description: 'Optional supplemental external URL.' },
      conductor_notes: { type: 'string', description: 'Optional replacement provenance notes.' },
      license: { type: 'string', description: 'Optional reader license id.' },
      ai_training: { type: 'string', description: 'Optional AI-training policy.' },
      revision_note: { type: 'string', minLength: 1, description: 'Short public note describing what changed.' },
    },
    required: ['title', 'abstract', 'authors', 'category', 'revision_note'],
    additionalProperties: false,
  };
}

const TOOLS = [
  // --- Read tools ---------------------------------------------------------
  {
    name: 'prexiv_search',
    description:
      'Full-text search across PreXiv manuscripts (title, abstract, authors, and extracted PDF body). Exact `prexiv:YYMMDD.xxxxxx` ids and DOIs match first. Returns a list of manuscript summaries.',
    inputSchema: {
      type: 'object',
      properties: {
        q: { type: 'string', description: 'Search query string.', minLength: 1 },
        limit: { type: 'integer', description: 'Optional cap on the number of results returned (max 50).', minimum: 1, maximum: 50 },
      },
      required: ['q'],
      additionalProperties: false,
    },
    handler: async ({ q, limit }) => {
      const data = await apiCall('GET', `/search?q=${encodeURIComponent(q)}`);
      const results = Array.isArray(data) ? data : (data?.items || data?.results || data?.manuscripts || []);
      return typeof limit === 'number' ? results.slice(0, limit) : results;
    },
  },
  {
    name: 'prexiv_browse',
    description:
      'List PreXiv manuscripts by ranking mode (`ranked` HN-style score/age decay, `new` reverse-chronological, `top` by all-time score, `audited` only audited submissions), optionally filtered by category and paginated.',
    inputSchema: {
      type: 'object',
      properties: {
        mode: { type: 'string', enum: ['ranked', 'new', 'top', 'audited'], description: 'Sort/filter mode. Default `ranked`.' },
        category: { type: 'string', description: 'Optional category id (e.g. `cs.LG`). Use prexiv_list_categories for the full list.' },
        page: { type: 'integer', minimum: 1, description: '1-indexed page number. Default 1.' },
        per: { type: 'integer', minimum: 1, maximum: 100, description: 'Items per page. Default determined by the API.' },
      },
      additionalProperties: false,
    },
    handler: async ({ mode, category, page, per }) => {
      const qs = new URLSearchParams();
      if (mode) qs.set('mode', mode);
      if (category) qs.set('category', category);
      if (page !== undefined) qs.set('page', String(page));
      if (per !== undefined) qs.set('per', String(per));
      const path = '/manuscripts' + (qs.toString() ? '?' + qs.toString() : '');
      return apiCall('GET', path);
    },
  },
  {
    name: 'prexiv_get',
    description:
      'Fetch a single manuscript by id. The id may be either the human-readable form `prexiv:YYMMDD.xxxxxx` or the numeric primary key. Returns the full record including conductor / auditor metadata, score, comment count, and external URL.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string', description: '`prexiv:YYMMDD.xxxxxx` id or numeric id of the manuscript.' },
      },
      required: ['id'],
      additionalProperties: false,
    },
    handler: async ({ id }) => apiCall('GET', `/manuscripts/${encodeURIComponent(id)}`),
  },
  {
    name: 'prexiv_get_comments',
    description:
      'Fetch the discussion thread for a manuscript. Returns a flat array of comments; each comment has a `parent_id` for client-side nesting.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string', description: '`prexiv:YYMMDD.xxxxxx` id or numeric id of the manuscript.' },
      },
      required: ['id'],
      additionalProperties: false,
    },
    handler: async ({ id }) => apiCall('GET', `/manuscripts/${encodeURIComponent(id)}/comments`),
  },
  {
    name: 'prexiv_list_categories',
    description:
      'List all valid manuscript categories. Each entry is `{ id, name }` (e.g. `{ "id": "cs.LG", "name": "Machine Learning" }`). The `id` is what `prexiv_submit`, `prexiv_revise`, and `prexiv_browse` expect.',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false },
    handler: async () => apiCall('GET', '/categories'),
  },

  // --- Write tools (require PREXIV_TOKEN) --------------------------------
  {
    name: 'prexiv_submit',
    description:
      'Submit a new manuscript to PreXiv. Requires `PREXIV_TOKEN` from a GitHub-, ORCID-, or email-verified account. Include title, abstract, authors, category, conductor metadata, and exactly one hosted artifact (`source_base64` with `source_filename`, or `pdf_base64` with `pdf_filename`). `external_url` is optional and supplemental.',
    requireAuth: true,
    inputSchema: manuscriptFieldsSchema({ allRequired: true }),
    handler: async (args) => apiCall('POST', '/manuscripts', args, { requireAuth: true }),
  },
  {
    name: 'prexiv_revise',
    description:
      'Publish a metadata revision for an existing manuscript. Requires `PREXIV_TOKEN` and you must own the manuscript (or be an admin). The current Rust API inherits the existing hosted PDF/source artifact for JSON revisions.',
    requireAuth: true,
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string', description: '`prexiv:YYMMDD.xxxxxx` id or numeric id of the manuscript to revise.' },
        ...revisionFieldsSchema().properties,
      },
      required: ['id', 'title', 'abstract', 'authors', 'category', 'revision_note'],
      additionalProperties: false,
    },
    handler: async ({ id, ...body }) =>
      apiCall('POST', `/manuscripts/${encodeURIComponent(id)}/versions`, body, { requireAuth: true }),
  },
  {
    name: 'prexiv_add_comment',
    description:
      'Post a comment on a manuscript. Requires `PREXIV_TOKEN`. Markdown plus inline `$..$` and display `$$..$$` LaTeX are supported. Pass `parent_id` to reply to an existing comment.',
    requireAuth: true,
    inputSchema: {
      type: 'object',
      properties: {
        manuscript_id: { type: 'string', description: '`prexiv:YYMMDD.xxxxxx` id or numeric id of the manuscript being commented on.' },
        content: { type: 'string', minLength: 1, description: 'Comment body. Markdown + LaTeX allowed.' },
        parent_id: { type: 'integer', description: 'Optional id of an existing comment to reply to.' },
      },
      required: ['manuscript_id', 'content'],
      additionalProperties: false,
    },
    handler: async ({ manuscript_id, content, parent_id }) => {
      const body = { content };
      if (parent_id !== undefined) body.parent_id = parent_id;
      return apiCall(
        'POST',
        `/manuscripts/${encodeURIComponent(manuscript_id)}/comments`,
        body,
        { requireAuth: true },
      );
    },
  },
  {
    name: 'prexiv_vote',
    description:
      'Cast an up- or down-vote on a manuscript. Requires `PREXIV_TOKEN`. Returns `{ ok, score }` after the change.',
    requireAuth: true,
    inputSchema: {
      type: 'object',
      properties: {
        manuscript_id: { type: 'string', description: 'Numeric id or `prexiv:YYMMDD.xxxxxx` manuscript id.' },
        value: { type: 'integer', enum: [1, -1], description: '1 to upvote, -1 to downvote.' },
      },
      required: ['manuscript_id', 'value'],
      additionalProperties: false,
    },
    handler: async ({ manuscript_id, value }) =>
      apiCall(
        'POST',
        `/manuscripts/${encodeURIComponent(String(manuscript_id))}/vote`,
        { value },
        { requireAuth: true },
      ),
  },
];

// Quick name->tool lookup.
const TOOL_BY_NAME = Object.fromEntries(TOOLS.map((t) => [t.name, t]));

// -----------------------------------------------------------------------------
// MCP server wiring
// -----------------------------------------------------------------------------

function buildServer() {
  const server = new Server(
    { name: 'prexiv-mcp', version: '0.1.0' },
    {
      capabilities: { tools: {} },
      instructions:
        'PreXiv is a research manuscript archive for manuscripts with explicit AI-use provenance. ' +
        'Read tools (search/browse/get/comments/categories) work without auth. ' +
        'Write tools (submit/revise/comment/vote) require PREXIV_TOKEN from an account verified through GitHub OAuth, ORCID OAuth, or email. ' +
        'Manuscript ids may be either numeric or the human-readable form `prexiv:YYMMDD.xxxxxx`. ' +
        'For categories see prexiv_list_categories.',
    },
  );

  server.setRequestHandler(ListToolsRequestSchema, async () => ({
    tools: TOOLS.map((t) => ({
      name: t.name,
      description: t.description,
      inputSchema: t.inputSchema,
    })),
  }));

  server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const { name, arguments: args } = request.params;
    const tool = TOOL_BY_NAME[name];
    if (!tool) {
      return {
        isError: true,
        content: [{ type: 'text', text: `Unknown tool: ${name}. Use tools/list to see available tools.` }],
      };
    }
    try {
      const result = await tool.handler(args || {});
      // MCP requires `content` blocks; we put a JSON-encoded text block.
      // Clients that understand structuredContent can also use it directly.
      const text = result === null || result === undefined ? '' : JSON.stringify(result, null, 2);
      const response = {
        content: [{ type: 'text', text }],
      };
      // structuredContent must be an object per the spec — only attach when applicable.
      if (result && typeof result === 'object' && !Array.isArray(result)) {
        response.structuredContent = result;
      }
      return response;
    } catch (err) {
      return {
        isError: true,
        content: [{ type: 'text', text: err?.message || String(err) }],
      };
    }
  });

  return server;
}

// -----------------------------------------------------------------------------
// Transport bootstrap
// -----------------------------------------------------------------------------

async function runStdio() {
  const server = buildServer();
  const transport = new StdioServerTransport();
  await server.connect(transport);
  // stdio servers run until stdin closes; nothing more to do here.
}

function isLoopbackBind(host) {
  const h = String(host || '').trim().toLowerCase();
  return h === '127.0.0.1' || h === 'localhost' || h === '::1' || h === '[::1]';
}

function constantTimeEqual(a, b) {
  if (typeof a !== 'string' || typeof b !== 'string') return false;
  const ab = Buffer.from(a);
  const bb = Buffer.from(b);
  if (ab.length !== bb.length) return false;
  let diff = 0;
  for (let i = 0; i < ab.length; i += 1) diff |= ab[i] ^ bb[i];
  return diff === 0;
}

function extractHttpAuthToken(req) {
  const direct = req.headers['x-mcp-auth-token'];
  if (typeof direct === 'string' && direct) return direct;
  const auth = req.headers.authorization;
  if (typeof auth !== 'string') return '';
  const m = auth.match(/^Bearer\s+(.+?)\s*$/i);
  return m ? m[1] : '';
}

function httpAuthorized(req) {
  if (!HTTP_AUTH_TOKEN) return true;
  return constantTimeEqual(extractHttpAuthToken(req), HTTP_AUTH_TOKEN);
}

async function runHttp() {
  if (!HTTP_AUTH_TOKEN && !isLoopbackBind(HOST)) {
    throw new Error(
      'Refusing to start HTTP MCP transport on a non-loopback MCP_HOST without MCP_HTTP_TOKEN. ' +
      'Set MCP_HTTP_TOKEN and send it as `Authorization: Bearer <token>` or `X-MCP-Auth-Token`.',
    );
  }

  // Lazy-load http transport so stdio runs don't pay its startup cost.
  const { StreamableHTTPServerTransport } = await import(
    '@modelcontextprotocol/sdk/server/streamableHttp.js'
  );
  const { randomUUID } = await import('node:crypto');
  const http = await import('node:http');

  // Stateful Streamable HTTP: one transport per session, identified by the
  // `mcp-session-id` request header. Multiple clients can be served at once.
  const transports = new Map(); // sessionId -> StreamableHTTPServerTransport

  const httpServer = http.createServer(async (req, res) => {
    if (req.url !== '/mcp') {
      res.statusCode = 404;
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify({ error: 'not found. POST/GET/DELETE to /mcp.' }));
      return;
    }

    if (!httpAuthorized(req)) {
      res.statusCode = 401;
      res.setHeader('Content-Type', 'application/json');
      res.setHeader('WWW-Authenticate', 'Bearer realm="prexiv-mcp"');
      res.end(JSON.stringify({ error: 'unauthorized' }));
      return;
    }

    // Read full body up-front (the SDK accepts a pre-parsed body).
    let body = undefined;
    if (req.method === 'POST') {
      const chunks = [];
      for await (const c of req) chunks.push(c);
      const raw = Buffer.concat(chunks).toString('utf8');
      if (raw) {
        try { body = JSON.parse(raw); }
        catch {
          res.statusCode = 400;
          res.setHeader('Content-Type', 'application/json');
          res.end(JSON.stringify({ error: 'invalid JSON in request body' }));
          return;
        }
      }
    }

    const sessionHeader = req.headers['mcp-session-id'];
    let transport = typeof sessionHeader === 'string' ? transports.get(sessionHeader) : undefined;

    // New session: only created on initialize requests.
    const isInitialize = body && body.method === 'initialize';
    if (!transport && isInitialize) {
      transport = new StreamableHTTPServerTransport({
        sessionIdGenerator: () => randomUUID(),
        onsessioninitialized: (sid) => transports.set(sid, transport),
      });
      transport.onclose = () => {
        if (transport.sessionId) transports.delete(transport.sessionId);
      };
      const server = buildServer();
      await server.connect(transport);
    } else if (!transport) {
      res.statusCode = 400;
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify({ error: 'no MCP session. Send an `initialize` request first.' }));
      return;
    }

    await transport.handleRequest(req, res, body);
  });

  await new Promise((resolve, reject) => {
    httpServer.once('error', reject);
    httpServer.listen(PORT, HOST, () => resolve());
  });
  // eslint-disable-next-line no-console
  console.error(`prexiv-mcp listening on http://${HOST}:${PORT}/mcp (Streamable HTTP)`);

  // Keep the process alive until killed.
}

async function main() {
  if (TRANSPORT === 'http') {
    await runHttp();
  } else if (TRANSPORT === 'stdio') {
    await runStdio();
  } else {
    // eslint-disable-next-line no-console
    console.error(`unknown MCP_TRANSPORT="${TRANSPORT}". Use "stdio" (default) or "http".`);
    process.exit(2);
  }
}

main().catch((err) => {
  // eslint-disable-next-line no-console
  console.error('prexiv-mcp fatal:', err?.stack || err);
  process.exit(1);
});
