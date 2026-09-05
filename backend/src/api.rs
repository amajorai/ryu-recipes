//! HTTP API for ghost recipes (`/api/recipes/*`).
//!
//! Surfaces the record / list / show / run / delete flow that gives Ryu's
//! workflow system ghost-os parity. Stateless ops hit the on-disk recipe store;
//! replay and the recording session go through the live ghost engine (via the
//! host). See the crate root for the transport split and rationale.
//!
//! The router is built with its own state ([`RecipesCtx`]) inside this crate so it
//! returns a state-less, mergeable `Router<()>`. Routes are declared relative to
//! `/api/recipes` (Core nests this service at that prefix behind the Recipes-App
//! gate), while the OpenAPI annotations keep the full external paths. Static
//! `record/*` segments are registered before `:name` so they match first (Axum
//! would otherwise capture `record` as a recipe name).

use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Router state for the recipes HTTP surface. Empty: the handlers reach the store
/// directly (via `ghost-core`) and the live ghost engine through the process-
/// global [`crate::RecipesHost`], so there is no per-request state to carry. Kept
/// as a named type so the router bakes a concrete state and returns `Router<()>`.
#[derive(Clone, Default)]
pub struct RecipesCtx;

impl RecipesCtx {
    pub fn new() -> Self {
        Self
    }
}

/// Build the `/api/recipes/*` router with its own state baked in, returning a
/// state-less `Router<()>` the host nests at `/api/recipes` behind the App gate.
pub fn routes(ctx: RecipesCtx) -> Router<()> {
    Router::new()
        .route("/record/start", post(record_start))
        .route("/record/status", get(record_status))
        .route("/record/stop", post(record_stop))
        .route("/", get(list_recipes).post(save_recipe))
        .route("/:name/run", post(run_recipe))
        .route("/:name", get(get_recipe).delete(delete_recipe))
        .with_state(ctx)
}

/// The OpenAPI sub-document for the recipes surface. Served by this sidecar (there
/// is no in-process merge into Core's spec — Core links none of this crate).
pub fn openapi() -> utoipa::openapi::OpenApi {
    <RecipesApiDoc as utoipa::OpenApi>::openapi()
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        delete_recipe,
        get_recipe,
        list_recipes,
        record_start,
        record_status,
        record_stop,
        run_recipe,
        save_recipe,
    ),
    components(schemas(SaveRecipeBody, RunRecipeBody, RecordStartBody))
)]
struct RecipesApiDoc;

/// Map an `anyhow::Error` to a 500 JSON body. Recipe failures are operational
/// (ghost not installed, recipe not found, malformed JSON), not request-shape
/// errors, so a uniform 500 with the message is the right surface.
fn err(status: StatusCode, e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": e.to_string() })))
}

/// `GET /api/recipes` — list installed recipes (summary form).
#[utoipa::path(
    get,
    path = "/api/recipes",
    tag = "Recipes",
    summary = "list installed recipes (summary form).",
    responses((status = 200, description = "OK", body = serde_json::Value))
)]
pub async fn list_recipes() -> (StatusCode, Json<Value>) {
    match crate::list() {
        Ok(recipes) => (StatusCode::OK, Json(json!({ "recipes": recipes }))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `GET /api/recipes/:name` — one recipe's full definition.
#[utoipa::path(
    get,
    path = "/api/recipes/{name}",
    tag = "Recipes",
    summary = "one recipe's full definition.",
    params(("name" = String, Path)),
    responses((status = 200, description = "OK", body = serde_json::Value))
)]
pub async fn get_recipe(Path(name): Path<String>) -> (StatusCode, Json<Value>) {
    match crate::get(&name) {
        Ok(recipe) => (StatusCode::OK, Json(json!({ "recipe": recipe }))),
        Err(e) => err(StatusCode::NOT_FOUND, e),
    }
}

/// Body for `POST /api/recipes`: a full recipe JSON document (ghost-os schema).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SaveRecipeBody {
    /// The recipe document itself, as a JSON object: `schema_version`, `name`,
    /// `description` and a `steps` array (the ghost-os recipe schema). Supply this
    /// or `recipe_json`, not neither.
    // Stays `Value`: the ghost-os recipe schema is owned by `ghost-core`'s validator,
    // versioned by `schema_version`, and re-declaring its step grammar here would give
    // the model a second, staler copy to trust.
    #[serde(default)]
    pub recipe: Option<Value>,
    /// The same recipe document pre-serialized to a JSON string. An alternative to
    /// `recipe` for callers that already hold the document as text.
    #[serde(default)]
    pub recipe_json: Option<String>,
}

/// `POST /api/recipes` — install (create or overwrite) a recipe.
#[utoipa::path(
    post,
    path = "/api/recipes",
    tag = "Recipes",
    summary = "install (create or overwrite) a recipe.",
    request_body = SaveRecipeBody,
    responses((status = 200, description = "OK", body = serde_json::Value))
)]
pub async fn save_recipe(Json(body): Json<SaveRecipeBody>) -> (StatusCode, Json<Value>) {
    let json_str = match (body.recipe, body.recipe_json) {
        (Some(v), _) => v.to_string(),
        (None, Some(s)) => s,
        (None, None) => {
            return err(
                StatusCode::BAD_REQUEST,
                "provide `recipe` (object) or `recipe_json` (string)",
            )
        }
    };
    match crate::save(&json_str) {
        Ok(recipe) => (
            StatusCode::OK,
            Json(json!({ "saved": true, "name": recipe.name, "recipe": recipe })),
        ),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// `DELETE /api/recipes/:name` — remove a recipe.
#[utoipa::path(
    delete,
    path = "/api/recipes/{name}",
    tag = "Recipes",
    summary = "remove a recipe.",
    params(("name" = String, Path)),
    responses((status = 200, description = "OK", body = serde_json::Value))
)]
pub async fn delete_recipe(Path(name): Path<String>) -> (StatusCode, Json<Value>) {
    match crate::delete(&name) {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "deleted": true, "name": name })),
        ),
        Err(e) => err(StatusCode::NOT_FOUND, e),
    }
}

/// Body for `POST /api/recipes/:name/run`: the parameter substitutions.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct RunRecipeBody {
    /// Values substituted into the recipe's `{{placeholder}}` slots, as a flat JSON
    /// object keyed by placeholder name. Omit (or send `{}`) for a recipe that takes
    /// no parameters.
    // Stays `Value`: the accepted keys are whatever the named recipe declares, so
    // there is no fixed schema to write down — a recipe-specific one would be wrong
    // for every other recipe.
    #[serde(default)]
    pub params: Value,
}

/// `POST /api/recipes/:name/run` — replay a recipe against native apps.
#[utoipa::path(
    post,
    path = "/api/recipes/{name}/run",
    tag = "Recipes",
    summary = "replay a recipe against native apps.",
    params(("name" = String, Path)),
    // Declared as the plain type even though the extractor is `Option<Json<..>>`.
    // `request_body = Option<T>` renders a nullable `oneOf` wrapper, which Core's
    // importer cannot resolve — the derived tool would be back to zero arguments. The
    // cost is that the body reads as required while the handler tolerates omitting it.
    request_body = RunRecipeBody,
    responses((status = 200, description = "OK", body = serde_json::Value))
)]
pub async fn run_recipe(
    Path(name): Path<String>,
    body: Option<Json<RunRecipeBody>>,
) -> (StatusCode, Json<Value>) {
    let params = body.map(|b| b.0.params).unwrap_or(Value::Null);
    match crate::run(&name, params).await {
        Ok(result) => (StatusCode::OK, Json(json!({ "result": result }))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Body for `POST /api/recipes/record/start`: the task being demonstrated.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RecordStartBody {
    /// What the user is about to demonstrate, in their own words (e.g. "export the
    /// weekly report from Numbers"). Stored as the recording's label. Optional.
    #[serde(default)]
    pub task: String,
}

/// `POST /api/recipes/record/start` — begin observing user input.
#[utoipa::path(
    post,
    path = "/api/recipes/record/start",
    tag = "Recipes",
    summary = "begin observing user input.",
    // Plain type, not `Option<RecordStartBody>` — see `run_recipe`.
    request_body = RecordStartBody,
    responses((status = 200, description = "OK", body = serde_json::Value))
)]
pub async fn record_start(body: Option<Json<RecordStartBody>>) -> (StatusCode, Json<Value>) {
    let task = body.map(|b| b.0.task).unwrap_or_default();
    match crate::record_start(&task).await {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

/// `GET /api/recipes/record/status` — poll the active recording.
#[utoipa::path(
    get,
    path = "/api/recipes/record/status",
    tag = "Recipes",
    summary = "poll the active recording.",
    responses((status = 200, description = "OK", body = serde_json::Value))
)]
pub async fn record_status() -> (StatusCode, Json<Value>) {
    match crate::record_status().await {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `POST /api/recipes/record/stop` — stop recording and return captured events.
#[utoipa::path(
    post,
    path = "/api/recipes/record/stop",
    tag = "Recipes",
    summary = "stop recording and return captured events.",
    // No `request_body`: the handler takes no extractor at all. Documenting one would
    // hand the derived tool an argument the endpoint ignores.
    responses((status = 200, description = "OK", body = serde_json::Value))
)]
pub async fn record_stop() -> (StatusCode, Json<Value>) {
    match crate::record_stop().await {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        env_lock, host_lock, install_fake_host, recipe_json, set_script, Script, TempStore,
    };
    use crate::{RecorderStarted, RecorderStopped};

    // Handlers are called directly and asserted on the `(StatusCode, Json<Value>)`
    // they return (`.0` = status, `.1 .0` = body value), so no HTTP transport /
    // tower is needed. Store-backed handlers run under `env_lock` + `TempStore`;
    // host-backed handlers under `host_lock` + the fake-host script.

    #[test]
    fn routes_and_openapi_build() {
        // The router assembles without panicking and the OpenAPI sub-doc lists the
        // recipe paths.
        let _router = routes(RecipesCtx::new());
        let spec = openapi();
        let paths = spec.paths.paths;
        assert!(paths.contains_key("/api/recipes"));
        assert!(paths.contains_key("/api/recipes/{name}"));
        assert!(paths.contains_key("/api/recipes/{name}/run"));
        assert!(paths.contains_key("/api/recipes/record/start"));
    }

    #[tokio::test]
    async fn list_recipes_returns_saved_rows() {
        let _g = env_lock();
        let _store = TempStore::new();
        crate::save(&recipe_json("alpha")).unwrap();
        let (status, Json(body)) = list_recipes().await;
        assert_eq!(status, StatusCode::OK);
        let rows = body["recipes"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], json!("alpha"));
    }

    #[tokio::test]
    async fn get_recipe_ok_and_not_found() {
        let _g = env_lock();
        let _store = TempStore::new();
        crate::save(&recipe_json("shown")).unwrap();

        let (status, Json(body)) = get_recipe(Path("shown".to_string())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["recipe"]["name"], json!("shown"));

        let (status, Json(body)) = get_recipe(Path("absent".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("absent"));
    }

    #[tokio::test]
    async fn save_recipe_accepts_object_body() {
        let _g = env_lock();
        let _store = TempStore::new();
        let body = SaveRecipeBody {
            recipe: Some(json!({
                "schema_version": 2, "name": "objsave", "description": "d", "steps": []
            })),
            recipe_json: None,
        };
        let (status, Json(out)) = save_recipe(Json(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["saved"], json!(true));
        assert_eq!(out["name"], json!("objsave"));
        assert!(crate::get("objsave").is_ok());
    }

    #[tokio::test]
    async fn save_recipe_accepts_stringified_body() {
        let _g = env_lock();
        let _store = TempStore::new();
        let body = SaveRecipeBody {
            recipe: None,
            recipe_json: Some(recipe_json("strsave")),
        };
        let (status, Json(out)) = save_recipe(Json(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["name"], json!("strsave"));
    }

    #[tokio::test]
    async fn save_recipe_rejects_empty_body() {
        // No env/store needed — the handler short-circuits before touching the store.
        let body = SaveRecipeBody {
            recipe: None,
            recipe_json: None,
        };
        let (status, Json(out)) = save_recipe(Json(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(out["error"].as_str().unwrap().contains("provide"));
    }

    #[tokio::test]
    async fn save_recipe_rejects_malformed_recipe() {
        let _g = env_lock();
        let _store = TempStore::new();
        let body = SaveRecipeBody {
            recipe: None,
            recipe_json: Some("{\"name\":\"broken\"}".to_string()),
        };
        let (status, Json(out)) = save_recipe(Json(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(out["error"].is_string());
    }

    #[tokio::test]
    async fn delete_recipe_ok_and_not_found() {
        let _g = env_lock();
        let _store = TempStore::new();
        crate::save(&recipe_json("temp")).unwrap();

        let (status, Json(out)) = delete_recipe(Path("temp".to_string())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["deleted"], json!(true));

        let (status, Json(out)) = delete_recipe(Path("temp".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(out["error"].is_string());
    }

    #[tokio::test]
    async fn run_recipe_ok_and_default_params() {
        let _g = host_lock();
        install_fake_host();
        set_script(Script {
            run_ok: Some(json!({ "content": [{ "type": "text", "text": "{\"done\":1}" }] })),
            ..Default::default()
        });
        // With an explicit body.
        let body = RunRecipeBody {
            params: json!({ "n": 1 }),
        };
        let (status, Json(out)) = run_recipe(Path("r".to_string()), Some(Json(body))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["result"], json!({ "done": 1 }));

        // With no body (params default to null) — still reaches the host.
        let (status, Json(out)) = run_recipe(Path("r".to_string()), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["result"], json!({ "done": 1 }));
    }

    #[tokio::test]
    async fn run_recipe_maps_host_error_to_500() {
        let _g = host_lock();
        install_fake_host();
        set_script(Script {
            run_err: Some("ghost down".to_string()),
            ..Default::default()
        });
        let (status, Json(out)) = run_recipe(Path("r".to_string()), None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(out["error"].as_str().unwrap().contains("ghost down"));
    }

    #[tokio::test]
    async fn record_start_handler_ok_and_conflict() {
        let _g = host_lock();
        install_fake_host();
        set_script(Script {
            start_ok: Some(RecorderStarted {
                started_at: "t0".to_string(),
                info: json!({}),
            }),
            ..Default::default()
        });
        let body = RecordStartBody {
            task: "do it".to_string(),
        };
        let (status, Json(out)) = record_start(Some(Json(body))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["task"], json!("do it"));

        // A host error (e.g. already-recording) maps to 409 CONFLICT.
        set_script(Script {
            start_err: Some("already recording".to_string()),
            ..Default::default()
        });
        let (status, Json(out)) = record_start(None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(out["error"].as_str().unwrap().contains("already recording"));
    }

    #[tokio::test]
    async fn record_status_handler_ok() {
        let _g = host_lock();
        install_fake_host();
        set_script(Script {
            status_ok: Some(None),
            ..Default::default()
        });
        let (status, Json(out)) = record_status().await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["recording"], json!(false));
    }

    #[tokio::test]
    async fn record_status_handler_maps_error_to_500() {
        let _g = host_lock();
        install_fake_host();
        set_script(Script {
            status_err: Some("poll failed".to_string()),
            ..Default::default()
        });
        let (status, Json(out)) = record_status().await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(out["error"].as_str().unwrap().contains("poll failed"));
    }

    #[tokio::test]
    async fn record_stop_handler_ok_and_bad_request() {
        let _g = host_lock();
        install_fake_host();
        set_script(Script {
            stop_ok: Some(RecorderStopped {
                task: "t".to_string(),
                started_at: "t0".to_string(),
                payload: json!({ "recording": false, "events": [] }),
            }),
            ..Default::default()
        });
        let (status, Json(out)) = record_stop().await;
        assert_eq!(status, StatusCode::OK);
        assert!(out["draft"].is_object());

        set_script(Script {
            stop_err: Some("no session".to_string()),
            ..Default::default()
        });
        let (status, Json(out)) = record_stop().await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(out["error"].as_str().unwrap().contains("no session"));
    }

    // ── OpenAPI document ───────────────────────────────────────────────────────

    /// This app's own manifest, read at compile time. The route contract lives there,
    /// so the invariants below compare the document against the real declaration
    /// rather than against a second list that could drift from it.
    fn openapi_manifest() -> serde_json::Value {
        serde_json::from_str(include_str!("../../manifest.json")).expect("valid JSON")
    }

    /// The manifest sidecar whose HTTP surface this router serves: the one that
    /// declares an `http.mount`. Selected BY mount rather than by index because an app
    /// may declare a second, mountless sidecar (finetune already does), and
    /// `sidecars[0]` would then quietly start asserting against the wrong process.
    fn mounted_sidecar() -> serde_json::Value {
        openapi_manifest()["sidecars"]
            .as_array()
            .expect("sidecars must be an array")
            .iter()
            .find(|s| s["http"]["mount"].is_string())
            .expect("one sidecar must declare an http.mount")
            .clone()
    }

    /// A manifest route (relative to the mount, in axum's `:param` form) rewritten
    /// into the form the OpenAPI document uses (absolute, in `{param}` form).
    ///
    /// The two forms differ ON PURPOSE — the router registers paths relative to the
    /// mount because Core nests it there, while the `#[utoipa::path]` annotations carry
    /// the absolute EXTERNAL path a caller actually hits. Normalise here; do not
    /// "align" either side.
    fn doc_path_for(mount: &str, route: &str) -> String {
        let joined = if route == "/" {
            mount.to_owned()
        } else {
            format!("{mount}{route}")
        };
        joined
            .split('/')
            .map(|seg| match seg.strip_prefix(':') {
                Some(name) => format!("{{{name}}}"),
                None => seg.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    #[test]
    fn openapi_doc_is_served_and_non_empty() {
        // The doc is no longer dead code: Core fetches it to derive tools.
        assert!(!super::openapi().paths.paths.is_empty());
    }

    #[test]
    fn every_declared_route_appears_in_the_openapi_doc() {
        // The direction that decides tool yield. Core's `ext_api::lower` keeps only the
        // document operations the manifest ALSO declares, so a declared route with no
        // `#[utoipa::path]` annotation is a tool that silently never exists — nothing
        // errors, the agent simply cannot call it. (The other direction is harmless: an
        // annotated path the manifest does not declare is dropped by the same filter.)
        let sidecar = mounted_sidecar();
        let mount = sidecar["http"]["mount"].as_str().expect("an http.mount");
        let doc = super::openapi();
        for route in sidecar["http"]["routes"]
            .as_array()
            .expect("routes must be an array")
        {
            let path = route["path"].as_str().expect("a route path");
            let expected = doc_path_for(mount, path);
            assert!(
                doc.paths.paths.contains_key(&expected),
                "'{path}' is declared in manifest.json but the OpenAPI document has no \
                 '{expected}' operation — Core derives no tool for it"
            );
        }
    }

    // ── Request-body schemas ───────────────────────────────────────────────────
    //
    // Core derives a write tool's ARGUMENTS from the operation's `requestBody`
    // schema. `request_body = serde_json::Value` documents an untyped body, so the
    // tool reaches the model with nothing it can fill in — discoverable and
    // uncallable. These tests pin the retrofit that replaced it.

    fn doc_json() -> Value {
        serde_json::to_value(super::openapi()).expect("the document serializes")
    }

    /// The JSON-schema node for one operation's request body, or `None` when the
    /// operation declares no body at all.
    fn request_body_schema<'a>(doc: &'a Value, path: &str, method: &str) -> Option<&'a Value> {
        let escaped = path.replace('/', "~1");
        doc.pointer(&format!(
            "/paths/{escaped}/{method}/requestBody/content/application~1json/schema"
        ))
    }

    #[test]
    fn post_routes_document_their_request_body() {
        let doc = doc_json();
        let schema = request_body_schema(&doc, "/api/recipes", "post")
            .expect("POST /api/recipes declares a request body");
        assert!(
            schema.get("$ref").is_some() || schema.get("properties").is_some(),
            "a derived write tool would have no arguments: {schema}"
        );
    }

    #[test]
    fn every_request_body_ref_resolves_against_components() {
        // The assertion above is necessary but not sufficient: a `$ref` to a type that
        // was never registered under `components.schemas` looks identical in the
        // operation and still yields ZERO arguments once Core resolves it. So walk
        // every operation and resolve for real. This is also what catches
        // `request_body = Option<T>`, which renders an unresolvable `oneOf` wrapper.
        let doc = doc_json();
        for (path, methods) in doc["paths"].as_object().expect("paths is an object") {
            for (method, op) in methods.as_object().expect("an operation map") {
                let Some(schema) = op.pointer("/requestBody/content/application~1json/schema")
                else {
                    continue;
                };
                let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
                    assert!(
                        schema.get("properties").is_some(),
                        "{method} {path} has a request body that is neither a $ref nor an \
                         object with properties — the derived tool gets no arguments: {schema}"
                    );
                    continue;
                };
                let name = reference
                    .strip_prefix("#/components/schemas/")
                    .unwrap_or_else(|| panic!("{method} {path}: unexpected $ref '{reference}'"));
                let target = doc
                    .pointer(&format!("/components/schemas/{name}"))
                    .unwrap_or_else(|| {
                        panic!(
                            "{method} {path} refs '{name}' but it is missing from \
                             components.schemas — add it to components(schemas(..))"
                        )
                    });
                assert!(
                    target.get("properties").is_some(),
                    "{method} {path} resolves to '{name}', which exposes no properties: {target}"
                );
                // And nothing INSIDE it may be a pointer either. Core resolves a `$ref`
                // one level into a schema, so a ref under `properties.x.items` or inside
                // a `oneOf` reaches the model as an opaque pointer — the same
                // zero-arguments failure, just one level down where the top-level checks
                // above cannot see it. Every nested type here is `#[schema(inline)]`d
                // precisely so this holds.
                assert!(
                    !target.to_string().contains("$ref"),
                    "{method} {path} → '{name}' carries a nested $ref Core cannot follow: {}",
                    serde_json::to_string_pretty(target).unwrap()
                );
            }
        }
    }

    #[test]
    fn optional_bodies_are_documented_as_the_plain_type() {
        // `run_recipe` and `record_start` take `Option<Json<..>>`. Writing
        // `request_body = Option<T>` for them would render `{"oneOf":[{"type":"null"},
        // {"$ref":..}]}` — no `$ref` at the top of the node, so Core's resolver walks
        // past it and the derived tool has zero arguments again. Assert the plain shape.
        let doc = doc_json();
        for (path, method, schema_name) in [
            ("/api/recipes/{name}/run", "post", "RunRecipeBody"),
            ("/api/recipes/record/start", "post", "RecordStartBody"),
        ] {
            let schema = request_body_schema(&doc, path, method)
                .unwrap_or_else(|| panic!("{method} {path} declares a request body"));
            assert_eq!(
                schema.get("$ref").and_then(Value::as_str),
                Some(format!("#/components/schemas/{schema_name}").as_str()),
                "{method} {path} must ref {schema_name} directly, not through a wrapper: {schema}"
            );
        }
    }

    #[test]
    fn body_field_docs_reach_the_schema_as_argument_descriptions() {
        // Field doc comments are lifted verbatim into `description`, which is the text
        // the model actually reads when deciding how to fill an argument.
        let doc = doc_json();
        let params = doc
            .pointer("/components/schemas/RunRecipeBody/properties/params/description")
            .and_then(Value::as_str)
            .expect("the `params` argument is described");
        assert!(
            params.contains("placeholder"),
            "the description must say what the keys are: {params}"
        );
        // `recipe` is a free-form `Value` on purpose (the ghost-os step grammar is owned
        // by the store's validator), so its description is the ONLY guidance the model
        // gets about what to put there. Losing it would leave an untyped, undocumented
        // argument — the worst of both.
        let recipe = doc
            .pointer("/components/schemas/SaveRecipeBody/properties/recipe/description")
            .and_then(Value::as_str)
            .expect("the free-form `recipe` argument is still described");
        assert!(
            recipe.contains("steps"),
            "the description must name the document's shape: {recipe}"
        );
    }

    #[test]
    fn body_less_routes_declare_no_request_body() {
        // `record/stop` takes no extractor at all. Documenting a body for it would
        // invent an argument the handler ignores.
        let doc = doc_json();
        assert!(
            request_body_schema(&doc, "/api/recipes/record/stop", "post").is_none(),
            "POST /api/recipes/record/stop must document no request body"
        );
    }
}
