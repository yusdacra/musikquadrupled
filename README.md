# musikquadrupled

A proxy server for musikcubed that implements TLS, alternative authentication solution and CORS for proper Web support. Compatible with musikcube.

# API (public)

All of the `musikcubed` APIs are implemented. On top of those:

- `share/generate/:external_id`: Returns a "scoped token" for the specified ID of a music. This allows one to access `share/*` APIs. The `token` query parameter must be present and must have a valid token, if not `401 Unauthorized` will be returned.
- `share/audio/:scoped_token`: Returns the music (or a range of it if `RANGE` header is present) corresponding to the specified `scoped_token`. Returns `404 Not Found` if the requested music isn't available.
- `share/info/:scoped_token`: Returns information (title, album, artist etc.) of the music corresponding to the specified `scoped_token` in JSON. Returns `404 Not Found` if the requested music isn't available.
- `share/thumbnail/:scoped_token`: Returns the thumbnail for the music corresponding to the specified `scoped_token`. Returns `404 Not Found` if the requested thumbnail isn't available.

# API (internal)

These are only exposed to `localhost` clients.

- `token/generate`: Generates a new token for use with the `token` query paramater in the public APIs.
- `token/revoke_all`: Revokes all tokens.