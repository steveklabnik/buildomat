/*
 * Copyright 2023 Oxide Computer Company
 */

use anyhow::{anyhow, bail, Result};
use buildomat_client::ext::*;
use buildomat_common::*;
use buildomat_github_database::types::*;
use chrono::prelude::*;
use dropshot::{
    endpoint, ConfigDropshot, HttpError, HttpResponseOk, RequestContext,
};
use schemars::JsonSchema;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use slog::{debug, error, info, o, trace, warn, Logger};
use std::collections::{HashMap, HashSet};
use std::result::Result as SResult;
use std::sync::Arc;
use std::time::Duration;

use super::{variety, App};

fn sign(body: &[u8], secret: &str) -> String {
    let hmac = hmac_sha256::HMAC::mac(body, secret.as_bytes());
    let mut out = "sha256=".to_string();
    for b in hmac.iter() {
        out.push_str(&format!("{:<02x}", b));
    }
    out
}

fn interr<T>(log: &slog::Logger, msg: &str) -> SResult<T, dropshot::HttpError> {
    error!(log, "internal error: {}", msg);
    Err(dropshot::HttpError::for_internal_error(msg.to_string()))
}

trait ToHttpError<T> {
    fn to_500(self) -> SResult<T, HttpError>;
}

impl<T> ToHttpError<T>
    for SResult<T, buildomat_github_database::DatabaseError>
{
    fn to_500(self) -> SResult<T, HttpError> {
        self.map_err(|e| {
            let msg = format!("internal error: {}", e);
            HttpError::for_internal_error(msg)
        })
    }
}

impl<T> ToHttpError<T> for Result<T> {
    fn to_500(self) -> SResult<T, HttpError> {
        self.map_err(|e| {
            let msg = format!("internal error: {}", e);
            HttpError::for_internal_error(msg)
        })
    }
}

impl<T> ToHttpError<T> for SResult<T, rusty_ulid::DecodingError> {
    fn to_500(self) -> SResult<T, HttpError> {
        self.map_err(|e| {
            let msg = format!("internal error: {}", e);
            HttpError::for_internal_error(msg)
        })
    }
}

impl<T, E> ToHttpError<T> for SResult<T, buildomat_client::Error<E>> {
    fn to_500(self) -> SResult<T, HttpError> {
        self.map_err(|e| {
            let msg = format!("internal error: {}", e.into_untyped());
            HttpError::for_internal_error(msg)
        })
    }
}

#[derive(Deserialize, JsonSchema)]
struct ArtefactPath {
    pub check_suite: String,
    pub url_key: String,
    pub check_run: String,
    pub output: String,
    pub name: String,
}

impl ArtefactPath {
    fn check_suite(&self) -> SResult<CheckSuiteId, HttpError> {
        self.check_suite.parse::<CheckSuiteId>().to_500()
    }

    fn check_run(&self) -> SResult<CheckRunId, HttpError> {
        self.check_run.parse::<CheckRunId>().to_500()
    }
}

#[derive(Deserialize, JsonSchema)]
struct ArtefactQuery {
    pub format: Option<String>,
}

#[endpoint {
    method = GET,
    path = "/artefact/{check_suite}/{url_key}/{check_run}/{output}/{name}"
}]
async fn artefact(
    rc: RequestContext<Arc<App>>,
    path: dropshot::Path<ArtefactPath>,
    query: dropshot::Query<ArtefactQuery>,
) -> SResult<hyper::Response<hyper::Body>, HttpError> {
    let app = rc.context();
    let path = path.into_inner();
    let query = query.into_inner();

    let cs = app.db.load_check_suite(&path.check_suite()?).to_500()?;
    let cr = app.db.load_check_run(&path.check_run()?).to_500()?;
    if cs.url_key != path.url_key {
        return interr(&rc.log, "url key mismatch");
    }

    let response = match cr.variety {
        CheckRunVariety::Basic => variety::basic::artefact(
            app,
            &cs,
            &cr,
            &path.output,
            &path.name,
            query.format.as_deref(),
        )
        .await
        .to_500()?,
        _ => None,
    };

    if let Some(response) = response {
        Ok(response)
    } else {
        let out = "<html><head><title>404 Not Found</title>\
            <body>Artefact not found!</body></html>";

        Ok(hyper::Response::builder()
            .status(hyper::StatusCode::NOT_FOUND)
            .header(hyper::header::CONTENT_TYPE, "text/html")
            .header(hyper::header::CONTENT_LENGTH, out.as_bytes().len())
            .body(hyper::Body::from(out))?)
    }
}

#[derive(Deserialize, JsonSchema)]
struct DetailsPath {
    pub check_suite: String,
    pub url_key: String,
    pub check_run: String,
}

impl DetailsPath {
    fn check_suite(&self) -> SResult<CheckSuiteId, HttpError> {
        self.check_suite.parse::<CheckSuiteId>().to_500()
    }

    fn check_run(&self) -> SResult<CheckRunId, HttpError> {
        self.check_run.parse::<CheckRunId>().to_500()
    }
}

#[derive(Deserialize, JsonSchema)]
struct DetailsQuery {
    pub ts: Option<String>,
}

#[endpoint {
    method = GET,
    path = "/details/{check_suite}/{url_key}/{check_run}",
}]
async fn details(
    rc: RequestContext<Arc<App>>,
    path: dropshot::Path<DetailsPath>,
    query: dropshot::Query<DetailsQuery>,
) -> SResult<hyper::Response<hyper::Body>, HttpError> {
    let app = rc.context();
    let path = path.into_inner();

    let query = query.into_inner();
    let local_time = query.ts.as_deref() == Some("all");

    let cs = app.db.load_check_suite(&path.check_suite()?).to_500()?;
    let cr = app.db.load_check_run(&path.check_run()?).to_500()?;
    if cs.url_key != path.url_key {
        return interr(&rc.log, "url key mismatch");
    }

    let mut out = String::new();
    out += "<html>\n";
    out += &format!("<head><title>Check Run: {}</title></head>\n", cr.name);
    out += "<body>\n";
    out += &format!("<h1>{}: {}</h1>\n", cr.id, cr.name);

    match cr.variety {
        CheckRunVariety::Control => {
            out += &variety::control::details(app, &cs, &cr, local_time)
                .await
                .to_500()?;
        }
        CheckRunVariety::FailFirst => {
            let p: super::FailFirstPrivate = cr.get_private().to_500()?;
            out += &format!("<pre>{:#?}</pre>\n", p);
        }
        CheckRunVariety::AlwaysPass => {
            let p: super::AlwaysPassPrivate = cr.get_private().to_500()?;
            out += &format!("<pre>{:#?}</pre>\n", p);
        }
        CheckRunVariety::Basic => {
            out += &variety::basic::details(app, &cs, &cr, local_time)
                .await
                .to_500()?;
        }
    }

    out += "</body>\n";
    out += "</html>\n";

    Ok(hyper::Response::builder()
        .status(hyper::StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "text/html")
        .header(hyper::header::CONTENT_LENGTH, out.as_bytes().len())
        .body(hyper::Body::from(out))?)
}

#[endpoint {
    method = POST,
    path = "/webhook",
}]
async fn webhook(
    rc: RequestContext<Arc<App>>,
    body: dropshot::UntypedBody,
) -> SResult<HttpResponseOk<()>, HttpError> {
    let app = rc.context();
    let log = &rc.log;

    /*
     * Locate the HMAC-256 signature of the body from Github.
     */
    let sig = {
        if let Some(h) = rc.request.headers().get("x-hub-signature-256") {
            if let Ok(s) = h.to_str() {
                s.to_string()
            } else {
                return interr(log, "invalid signature header");
            }
        } else {
            return interr(log, "no signature header");
        }
    };

    /*
     * Fetch the body as raw bytes so that we can calculate the signature before
     * parsing it as JSON.
     */
    let buf = body.as_bytes();
    let oursig = sign(buf, &app.config.webhook_secret);

    if sig != oursig {
        error!(log, "signatures"; "theirs" => sig, "ours" => oursig);
        return interr(log, "signature mismatch");
    }

    let v: serde_json::Value = if let Ok(ok) = serde_json::from_slice(buf) {
        ok
    } else {
        return interr(log, "invalid JSON");
    };

    /*
     * Save the headers as well.
     */
    let mut headers = HashMap::new();
    for (k, v) in rc.request.headers().iter() {
        trace!(log, "header: {} -> {:?}", k, v);
        headers.insert(k.to_string(), v.to_str().unwrap().to_string());
    }

    let uuid = if let Some(uuid) = headers.get("x-github-delivery") {
        uuid.as_str()
    } else {
        return interr(log, "missing delivery uuid");
    };
    let event = if let Some(event) = headers.get("x-github-event") {
        event.as_str()
    } else {
        return interr(log, "missing delivery event");
    };

    trace!(log, "from GitHub: {:#?}", v);

    let then = Utc::now();

    let (seq, new_delivery) = loop {
        match app.db.store_delivery(uuid, event, &headers, &v, then) {
            Ok(del) => break del,
            Err(e) if e.is_locked_database() => {
                /*
                 * Clients under our control will retry on failures, but
                 * generally GitHub will not retry a failed delivery.  If the
                 * database is locked by another process, sleep and try again
                 * until we succeed.
                 */
                warn!(log, "delivery uuid {uuid} sleeping for lock..");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(e) => return interr(log, &format!("storing delivery: {e}")),
        }
    };

    if new_delivery {
        info!(log, "stored as delivery seq {seq} uuid {uuid}");
    } else {
        warn!(log, "replayed delivery seq {seq} uuid {uuid}");
    }

    Ok(HttpResponseOk(()))
}

#[endpoint {
    method = GET,
    path = "/status",
}]
async fn status(
    rc: RequestContext<Arc<App>>,
) -> SResult<hyper::Response<hyper::Body>, HttpError> {
    let app = rc.context();
    let b = app.buildomat_admin();

    let mut out = String::new();
    out += "<html>\n";
    out += "<head><title>Buildomat Status</title></head>\n";
    out += "<body>\n";
    out += "<h1>Buildomat Status</h1>\n";

    /*
     * Load active jobs, recently completed jobs, and active workers:
     */
    let jobs = b.admin_jobs_get().active(true).send().await.to_500()?;
    let oldjobs = {
        let mut oldjobs =
            b.admin_jobs_get().completed(40).send().await.to_500()?;
        /*
         * Display most recent job first by sorting the ID backwards; a ULID
         * begins with a timestamp prefix, so a lexicographical sort is ordered
         * by creation time.
         */
        oldjobs.sort_by(|a, b| b.id.cmp(&a.id));
        oldjobs
    };
    let workers = b.workers_list().active(true).send().await.to_500()?;
    let targets = b
        .targets_list()
        .send()
        .await
        .to_500()?
        .iter()
        .map(|t| (t.id.to_string(), t.name.to_string()))
        .collect::<HashMap<String, String>>();
    let mut users: HashMap<String, String> = Default::default();

    fn github_url(tags: &HashMap<String, String>) -> Option<String> {
        let owner = tags.get("gong.repo.owner")?;
        let name = tags.get("gong.repo.name")?;
        let checkrun = tags.get("gong.run.github_id")?;

        let url =
            format!("https://github.com/{}/{}/runs/{}", owner, name, checkrun);

        Some(format!("<a href=\"{}\">{}</a>", url, url))
    }

    fn commit_url(tags: &HashMap<String, String>) -> Option<String> {
        let owner = tags.get("gong.repo.owner")?;
        let name = tags.get("gong.repo.name")?;
        let sha = tags.get("gong.head.sha")?;

        let url =
            format!("https://github.com/{}/{}/commit/{}", owner, name, sha);

        Some(format!("<a href=\"{}\">{}</a>", url, sha))
    }

    fn github_info(tags: &HashMap<String, String>) -> Option<String> {
        let owner = tags.get("gong.repo.owner")?;
        let name = tags.get("gong.repo.name")?;
        let title = tags.get("gong.name")?;

        let url = format!("https://github.com/{}/{}", owner, name);

        let mut out = format!("<a href=\"{}\">{}/{}</a>", url, owner, name);
        if let Some(branch) = tags.get("gong.head.branch") {
            out.push_str(&format!(" ({})", branch));
        }
        out.push_str(&format!(": {}", title));

        Some(out)
    }

    fn dump_info(job: &buildomat_client::types::Job) -> String {
        let tags = &job.tags;

        let mut out = String::new();
        if let Some(info) = github_info(tags) {
            out += &format!("&nbsp;&nbsp;&nbsp;<b>{}</b><br>\n", info);
        }
        if let Some(url) = commit_url(tags) {
            out += &format!("&nbsp;&nbsp;&nbsp;<b>commit:</b> {}<br>\n", url);
        }
        if let Some(url) = github_url(tags) {
            out += &format!("&nbsp;&nbsp;&nbsp;<b>url:</b> {}<br>\n", url);
        }
        if job.target == job.target_real {
            out += &format!(
                "&nbsp;&nbsp;&nbsp;<b>target:</b> {}<br>\n",
                job.target
            );
        } else {
            out += &format!(
                "&nbsp;&nbsp;&nbsp;<b>target:</b> {} &rarr; {}<br>\n",
                job.target, job.target_real
            );
        }

        if let Some(t) = job.times.get("complete") {
            out += &format!(
                "&nbsp;&nbsp;&nbsp;<b>completed at:</b> {} ({} ago)<br>\n",
                t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                t.age().render(),
            );
        } else if let Some(t) = job.times.get("submit") {
            out += &format!(
                "&nbsp;&nbsp;&nbsp;<b>submitted at:</b> {} ({} ago)<br>\n",
                t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                t.age().render(),
            );
        } else if let Ok(id) = job.id() {
            let t = id.creation();
            out += &format!(
                "&nbsp;&nbsp;&nbsp;<b>submitted at:</b> {} ({} ago)<br>\n",
                t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                t.age().render(),
            );
        }

        let mut times = Vec::new();
        if let Some(t) = job.duration("submit", "ready") {
            times.push(format!("waited {}", t.render()));
        }
        if let Some(t) = job.duration("ready", "assigned") {
            times.push(format!("queued {}", t.render()));
        }
        if let Some(t) = job.duration("assigned", "complete") {
            times.push(format!("ran for {}", t.render()));
        }
        if !times.is_empty() {
            out += &format!(
                "&nbsp;&nbsp;&nbsp;<b>times:</b> {}<br>\n",
                times.join(", ")
            );
        }

        if !out.is_empty() {
            out = format!("<br>\n{}\n", out);
        }
        out
    }

    let mut seen = HashSet::new();

    if workers.workers.iter().any(|w| !w.deleted) {
        out += "<h2>Active Workers</h2>\n";
        out += "<ul>\n";

        for w in workers.workers.iter() {
            if w.deleted {
                continue;
            }

            out += "<li>";
            out += &w.id;
            let mut things = Vec::new();
            if let Some(t) = targets.get(&w.target) {
                things.push(t.to_string());
            }
            if let Some(fp) = &w.factory_private {
                things.push(fp.to_string());
            }
            if !things.is_empty() {
                out += &format!(" ({})", things.join(", "));
            }
            out += &format!(
                " created {} ({} ago)\n",
                w.id()
                    .to_500()?
                    .creation()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                w.id().to_500()?.age().render(),
            );

            if !w.jobs.is_empty() {
                out += "<ul>\n";

                for job in w.jobs.iter() {
                    seen.insert(job.id.to_string());

                    if !users.contains_key(&job.owner) {
                        let owner = b
                            .user_get()
                            .user(&job.owner)
                            .send()
                            .await
                            .to_500()?;
                        users.insert(job.owner.clone(), owner.name.to_string());
                    }

                    out += "<li>";
                    out += &format!(
                        "job {} user {}",
                        job.id,
                        users.get(&job.owner).unwrap()
                    );
                    if let Some(job) = jobs.iter().find(|j| j.id == job.id) {
                        out += &dump_info(&job);
                    }
                    out += "<br>\n";
                }

                out += "</ul>\n";
            }
        }

        out += "</ul>\n";
    }

    for (heading, state) in [
        ("Queued Jobs (waiting for capacity)", Some("queued")),
        ("Waiting Jobs (waiting for a dependency)", Some("waiting")),
        ("Other Jobs", None),
    ] {
        let mut did_heading = false;

        for job in jobs.iter() {
            if seen.contains(&job.id) {
                continue;
            }

            let display = if job.state == "completed" || job.state == "failed" {
                /*
                 * Completed jobs will be displayed in a later section.
                 */
                false
            } else if let Some(state) = state.as_deref() {
                /*
                 * This round, we are displaying jobs of a particular status.
                 */
                state == &job.state
            } else {
                /*
                 * Catch all the stragglers.
                 */
                true
            };

            if !display {
                continue;
            }

            seen.insert(job.id.to_string());

            if !did_heading {
                did_heading = true;
                out += &format!("<h2>{}</h2>\n", heading);
                out += "<ul>\n";
            }

            if !users.contains_key(&job.owner) {
                let owner =
                    b.user_get().user(&job.owner).send().await.to_500()?;
                users.insert(job.owner.clone(), owner.name.to_string());
            }

            out += "<li>";
            out +=
                &format!("{} user {}", job.id, users.get(&job.owner).unwrap());
            out += &dump_info(&job);
            out += "<br>\n";
        }

        if did_heading {
            out += "</ul>\n";
        }
    }

    out += "<h2>Recently Completed Jobs</h2>\n";
    out += "<ul>\n";
    for job in oldjobs.iter() {
        if seen.contains(&job.id) {
            continue;
        }

        if !users.contains_key(&job.owner) {
            let owner = b.user_get().user(&job.owner).send().await.to_500()?;
            users.insert(job.owner.clone(), owner.name.to_string());
        }

        out += "<li>";
        out += &format!("{} user {}", job.id, users.get(&job.owner).unwrap());
        let (colour, word) = if job.state == "failed" {
            if job.cancelled {
                ("dabea6", "CANCEL")
            } else {
                ("f29494", "FAIL")
            }
        } else {
            ("97f294", "OK")
        };
        out += &format!(
            " <span style=\"background-color: #{}\">[{}]</span>",
            colour, word
        );
        out += &dump_info(&job);
        out += "<br>\n";
    }
    out += "</ul>\n";

    out += "</body>\n";
    out += "</html>\n";

    Ok(hyper::Response::builder()
        .status(hyper::StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(hyper::header::CONTENT_LENGTH, out.as_bytes().len())
        .body(hyper::Body::from(out))?)
}

#[derive(Deserialize, JsonSchema)]
struct PublishedFilePath {
    pub owner: String,
    pub repo: String,
    pub series: String,
    pub version: String,
    pub name: String,
}

#[endpoint {
    method = GET,
    path = "/public/file/{owner}/{repo}/{series}/{version}/{name}",
}]
async fn published_file(
    rc: RequestContext<Arc<App>>,
    path: dropshot::Path<PublishedFilePath>,
) -> SResult<hyper::Response<hyper::Body>, HttpError> {
    let app = rc.context();
    let path = path.into_inner();

    /*
     * Determine the buildomat username for this GitHub owner/repository:
     */
    let bmu = if let Some(repo) =
        app.db.lookup_repository(&path.owner, &path.repo).to_500()?
    {
        app.buildomat_username(&repo)
    } else {
        let out = "<html><head><title>404 Not Found</title>\
            <body>Artefact not found!</body></html>";

        return Ok(hyper::Response::builder()
            .status(hyper::StatusCode::NOT_FOUND)
            .header(hyper::header::CONTENT_TYPE, "text/html")
            .header(hyper::header::CONTENT_LENGTH, out.as_bytes().len())
            .body(hyper::Body::from(out))?);
    };

    let b = app.buildomat_admin();

    let backend = b
        .public_file_download()
        .username(&bmu)
        .series(&path.series)
        .version(&path.version)
        .name(&path.name)
        .send()
        .await
        .to_500()?;

    let ct = guess_mime_type(&path.name);
    let cl = backend.content_length().unwrap();

    Ok(hyper::Response::builder()
        .status(hyper::StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, ct)
        .header(hyper::header::CONTENT_LENGTH, cl)
        .body(hyper::Body::wrap_stream(backend.into_inner_stream()))?)
}

#[derive(Deserialize, JsonSchema)]
struct BranchToCommitPath {
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

#[endpoint {
    method = GET,
    path = "/public/branch/{owner}/{repo}/{branch}",
}]
async fn branch_to_commit(
    rc: RequestContext<Arc<App>>,
    path: dropshot::Path<BranchToCommitPath>,
) -> SResult<hyper::Response<hyper::Body>, HttpError> {
    let app = rc.context();
    let path = path.into_inner();

    /*
     * Make sure we know about this repository before we even bother to look it
     * up.
     */
    let Some(repo) =
        app.db.lookup_repository(&path.owner, &path.repo).to_500()? else
    {
        let out = "<html><head><title>404 Not Found</title>\
            <body>Not found!</body></html>";

        return Ok(hyper::Response::builder()
            .status(hyper::StatusCode::NOT_FOUND)
            .header(hyper::header::CONTENT_TYPE, "text/html")
            .header(hyper::header::CONTENT_LENGTH, out.as_bytes().len())
            .body(hyper::Body::from(out))?);
    };

    /*
     * We need to use the credentials for the installation owned by the user
     * that owns this repo:
     */
    let install = app.db.repo_to_install(&repo).map_err(|e| {
        HttpError::for_internal_error(format!("repo {repo:?} to install: {e}"))
    })?;

    let branch = app
        .install_client(install.id)
        .repos()
        .get_branch(&repo.owner, &repo.name, &path.branch)
        .await
        .map_err(|e| HttpError::for_internal_error(e.to_string()))?;

    let body = format!("{}\n", branch.commit.sha);

    Ok(hyper::Response::builder()
        .status(hyper::StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "text/plain")
        .header(hyper::header::CONTENT_LENGTH, body.as_bytes().len())
        .body(body.into())?)
}

pub(crate) async fn server(
    app: Arc<App>,
    bind_address: std::net::SocketAddr,
) -> Result<()> {
    let cd = ConfigDropshot {
        bind_address,
        request_body_max_bytes: 1024 * 1024,
        ..Default::default()
    };

    let mut api = dropshot::ApiDescription::new();
    api.register(webhook).unwrap();
    api.register(details).unwrap();
    api.register(artefact).unwrap();
    api.register(status).unwrap();
    api.register(published_file).unwrap();
    api.register(branch_to_commit).unwrap();

    let log = app.log.clone();
    let s = dropshot::HttpServerStarter::new(&cd, api, app, &log)
        .map_err(|e| anyhow!("server starter error: {:?}", e))?;

    s.start().await.map_err(|e| anyhow!("HTTP server failure: {}", e))?;
    bail!("HTTP server exited unexpectedly");
}
