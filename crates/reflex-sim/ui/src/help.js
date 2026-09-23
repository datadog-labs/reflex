// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

export const helpIntroHtml = "Clients send requests through a gateway to <strong>Catalog, Payments, and Search</strong>, each with its own circuit breaker.";
export const helpDetailsHtml = "<p>Increase traffic or inject slowdowns and errors. Opening a circuit blocks new requests so outstanding work can drain.</p>\n<p>Each circuit has three states:</p>\n<ul>\n<li><strong>Closed:</strong> Requests flow normally.</li>\n<li><strong>Open:</strong> New requests are blocked.</li>\n<li><strong>Probe:</strong> One request tests recovery. Success closes the circuit; an error or timeout reopens it.</li>\n</ul>\n<p>Jev uses recent request outcomes to recommend an action: <strong>open, probe, or no change</strong>. Reflex checks the rules before applying it: opening requires 10 responses within five simulated seconds; probing requires a three-second cooldown and allows one request at a time. Stale recommendations and responses from earlier circuit generations cannot change the state.</p>";
export const helpHtml = `<p>${helpIntroHtml}</p>${helpDetailsHtml}`;
