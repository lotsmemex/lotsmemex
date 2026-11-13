# memex.agent_api.read

The `/api/v1` surface is read-only and shares the exact process-level
sensitivity ceiling with HTML. All success responses have a versioned envelope;
hidden and unknown resources both return the same `not_found` response. No CORS
header is emitted.
