import http from "k6/http";
import { check, sleep } from "k6";

// Drives POST /emails against the catapulte service over the compose network.
// Docker's round-robin DNS spreads connections across the scaled replicas.
//
// Modes (K6_MODE):
//   ramp  (default) — open-ended ramping VUs; measures peak submit throughput.
//   fixed           — a bounded total (K6_ITERS, default 6000) of submissions.
//
// When K6_MODE=fixed AND MAILPIT_URL is set, teardown() waits for the async send
// pipeline to drain and asserts exactly-once delivery across the replicas
// (delivered == submitted, unique recipients == submitted: no loss, no dupes).
const MODE = __ENV.K6_MODE || "ramp";
const ITERS = parseInt(__ENV.K6_ITERS || "6000", 10);
const VUS = parseInt(__ENV.K6_VUS || "30", 10);
const MAILPIT = __ENV.MAILPIT_URL || "";
const DRAIN_BUDGET_S = parseInt(__ENV.K6_DRAIN_BUDGET_S || "540", 10);

const scenarios =
  MODE === "fixed"
    ? {
        submit: {
          executor: "shared-iterations",
          vus: VUS,
          iterations: ITERS,
          maxDuration: "120s",
        },
      }
    : {
        submit: {
          executor: "ramping-vus",
          startVUs: 0,
          stages: [
            { duration: "10s", target: 20 },
            { duration: "30s", target: 20 },
            { duration: "5s", target: 0 },
          ],
          gracefulStop: "5s",
        },
      };

export const options = {
  scenarios,
  teardownTimeout: __ENV.K6_TEARDOWN_TIMEOUT || "10m",
  thresholds: {
    // Informational: a breach is reported, not fatal to the run's usefulness.
    http_req_failed: ["rate<0.01"],
    http_req_duration: ["p(95)<500"],
  },
};

const URL = __ENV.TARGET_URL || "http://catapulte:3000/emails";
const PARAMS = { headers: { "Content-Type": "application/json" } };

export default function () {
  const payload = JSON.stringify({
    sender: "load@example.com",
    recipients: [{ kind: "to", address: `rcpt-${__VU}-${__ITER}@example.com` }],
    subject: "k6 load",
    body: { kind: "plain", text: "hello from k6" },
  });
  const res = http.post(URL, payload, PARAMS);
  check(res, {
    "status is 200": (r) => r.status === 200,
    "has id": (r) => {
      try {
        return typeof r.json("id") === "string";
      } catch (_) {
        return false;
      }
    },
  });
}

function mailpitTotal() {
  const r = http.get(`${MAILPIT}/api/v1/messages?limit=1`);
  try {
    return r.json("total") || 0;
  } catch (_) {
    return 0;
  }
}

export function teardown() {
  if (MODE !== "fixed" || !MAILPIT) {
    console.log("delivery verification skipped (needs K6_MODE=fixed and MAILPIT_URL)");
    return;
  }
  const expected = ITERS;
  const deadline = Date.now() + DRAIN_BUDGET_S * 1000;
  let total = 0;
  let prev = -1;
  let stable = 0;
  while (Date.now() < deadline) {
    total = mailpitTotal();
    if (total >= expected) break;
    stable = total === prev ? stable + 1 : 0;
    if (stable >= 15) {
      console.log(`drain stalled at ${total}/${expected}`);
      break;
    }
    prev = total;
    sleep(2);
  }
  // Pull all delivered messages and count distinct recipients (each submission
  // used a unique rcpt-<vu>-<iter> address, so distinct == submitted ⇒ no dupes).
  const all = http.get(`${MAILPIT}/api/v1/messages?limit=${expected + 100}`);
  let unique = 0;
  try {
    const msgs = all.json("messages") || [];
    unique = new Set(
      msgs.map((m) => (m.To && m.To[0] && m.To[0].Address) || ""),
    ).size;
  } catch (_) {
    /* leave unique = 0 */
  }
  console.log(
    `DELIVERY: submitted=${expected} delivered=${total} unique_recipients=${unique}`,
  );
  check(null, {
    "delivered == submitted (no loss)": () => total === expected,
    "unique recipients == submitted (no duplicates)": () => unique === expected,
  });
}
