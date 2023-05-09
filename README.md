# musikquadrupled

A proxy server for musikcubed that implements TLS, alternative authentication solution, CORS for proper Web support and a few extra goodies. Compatible with musikcube.

### Configuration

Configuration is done via `MUSIKQUAD_*` environment variables. Alternatively you can also provide a `.env` file in the same directory as the binary or a parent directory, which will be readed and loaded as environment variables.

See `.env.example` for a commented `.env` file.

### API (public)

All of the `musikcubed` APIs are implemented. On top of those:

- `share/generate/:external_id`: Returns a "scoped token" for the specified ID of a music. This allows one to access `share/*` APIs. This endpoint requires authentication.
- `share/audio/:scoped_token`: Returns the music (or a range of it if `RANGE` header is present) corresponding to the specified `scoped_token`. Returns `404 Not Found` if the requested music isn't available. Returns `401 Unauthorized` if `scoped_token` is invalid.
- `share/info/:scoped_token`: Returns information (title, album, artist etc.) of the music corresponding to the specified `scoped_token` in JSON. Returns `404 Not Found` if the requested music isn't available. Returns `401 Unauthorized` if `scoped_token` is invalid.
- `share/thumbnail/:scoped_token`: Returns the thumbnail for the music corresponding to the specified `scoped_token`. Returns `404 Not Found` if the requested thumbnail isn't available. Returns `401 Unauthorized` if `scoped_token` is invalid.

If an endpoint requires authentication this means you have to provide a token via a `token` query paramater in the URL. If not provided `401 Unauthorized` will be returned. For `musikcube`, this is not necessary as the server will also handle `Authentication` header for the APIs `musikcube` uses.

### API (internal)

These are only exposed to `localhost` clients.

- `token/generate`: Generates a new token for use with the `token` query paramater in the public APIs.
- `token/revoke_all`: Revokes all tokens.
