use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::SocketAddr;
use warp::Filter;

/// Wrapper to pretty-print optional values.
struct OptFmt<T>(Option<T>);

impl<T: fmt::Display> fmt::Display for OptFmt<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref t) = self.0 {
            fmt::Display::fmt(t, f)
        } else {
            f.write_str("-")
        }
    }
}

#[derive(Serialize, Deserialize)]
struct HistoryQuery {
    error_backtraces: Option<bool>,
    error_causes: Option<bool>,
}

pub(crate) async fn run_server(addr: SocketAddr, state: crate::State) -> Result<()> {
    let api = filters::api(state);

    let routes = api.with(warp::log::custom(move |info: warp::log::Info<'_>| {
        // This is the exact same as warp::log::log("api"), but logging at DEBUG instead of INFO.
        log::debug!(
            target: "api",
            "{} \"{} {} {:?}\" {} \"{}\" \"{}\" {:?}",
            OptFmt(info.remote_addr()),
            info.method(),
            info.path(),
            info.version(),
            info.status().as_u16(),
            OptFmt(info.referer()),
            OptFmt(info.user_agent()),
            info.elapsed(),
        );
    }));

    // Start up the server...
    let (_, fut) = warp::serve(routes)
        .try_bind_ephemeral(addr)
        .context("unable to bind")?;
    fut.await;

    Ok(())
}

mod filters {
    use super::{handlers, HistoryQuery};
    use crate::State;
    use warp::Filter;

    pub(crate) fn api(
        state: State,
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("api" / "v1" / ..).and(history(state))
    }

    pub(crate) fn history(
        state: State,
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("history" / ..).and(history_alias(state.clone()))
    }

    pub(crate) fn history_alias(
        state: State,
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("alias" / ..)
            .and(warp::path::param())
            .and(warp::get())
            .and(warp::query::<HistoryQuery>())
            .and(with_state(state))
            .and_then(handlers::get_history_alias)
    }

    fn with_state(
        state: State,
    ) -> impl Filter<Extract = (State,), Error = std::convert::Infallible> + Clone {
        warp::any().map(move || state.clone())
    }
}

mod handlers {
    use super::HistoryQuery;
    use crate::State;
    use log::debug;
    use warp::{reject, Rejection};

    pub(crate) async fn get_history_alias(
        alias: String,
        query: HistoryQuery,
        state: State,
    ) -> Result<impl warp::Reply, Rejection> {
        let include_backtraces = query.error_backtraces.or_else(|| Some(false)).unwrap();
        let include_causes = query.error_causes.or_else(|| Some(false)).unwrap();

        let mut history = match state.lock().await.get(&alias) {
            None => {
                debug!("alias {} unknown", alias);
                Err(reject::not_found())
            }
            Some((_, history)) => Ok(history.lock().await.clone()),
        }?;

        if !include_backtraces {
            // Strip backtraces
            history.iter_mut().for_each(|e| {
                if let Err(err) = &mut e.inner {
                    err.backtrace = None;
                }
            })
        }

        if !include_causes {
            // Strip causes
            history.iter_mut().for_each(|e| {
                if let Err(err) = &mut e.inner {
                    err.chain = Vec::new();
                }
            })
        }

        Ok(warp::reply::json(&history))
    }
}
