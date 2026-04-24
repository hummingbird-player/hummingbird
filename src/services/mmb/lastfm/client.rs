use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use super::types::{GetSession, GetToken, Session};

pub struct LastFMClient {
    client: zed_reqwest::Client,
    endpoint: url::Url,
    api_key: String,
    api_secret: String,
    auth_session: Option<String>,
}

impl LastFMClient {
    pub fn new(api_key: String, api_secret: String) -> Self {
        LastFMClient {
            api_key,
            api_secret,
            auth_session: None,
            endpoint: "https://ws.audioscrobbler.com/2.0".parse().unwrap(),
            client: zed_reqwest::Client::builder()
                .user_agent("HummingbirdMMBS/1.0")
                .build()
                .unwrap(),
        }
    }

    pub fn from_global() -> Option<Self> {
        let &(key, secret) = super::LASTFM_CREDS.as_ref()?;
        Some(LastFMClient::new(key.to_owned(), secret.to_owned()))
    }

    pub fn set_session(&mut self, session: String) {
        self.auth_session = Some(session);
    }

    fn get<'a>(
        &'a self,
        params: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> zed_reqwest::RequestBuilder {
        let params: BTreeMap<_, _> = Some(("api_key", &*self.api_key))
            .into_iter()
            .chain(params)
            .collect();

        let mut req = self
            .client
            .get(self.endpoint.clone())
            .query(&[("format", "json")]);
        let mut sig = md5::Context::new();
        for (k, v) in params {
            req = req.query(&[(k, v)]);
            sig.consume(k);
            sig.consume(v);
        }
        sig.consume(&self.api_secret);

        req.query(&[("api_sig", format_args!("{:x}", sig.finalize()))])
    }

    fn post<'a>(
        &'a self,
        params: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> zed_reqwest::RequestBuilder {
        let mut req = self.get(params).build().unwrap();
        *req.method_mut() = zed_reqwest::Method::POST;
        *req.body_mut() = req
            .url()
            .query()
            .unwrap()
            .strip_prefix("format=json&")
            .map(String::from)
            .map(zed_reqwest::Body::wrap);
        req.url_mut()
            .query_pairs_mut()
            .clear()
            .append_pair("format", "json");
        zed_reqwest::RequestBuilder::from_parts(self.client.clone(), req)
    }

    pub async fn get_token(&mut self) -> anyhow::Result<String> {
        let req = self.get([("method", "auth.gettoken")]);
        let GetToken { token } = req.send().await?.json().await?;
        Ok(token)
    }

    pub async fn get_session(&mut self, token: &str) -> anyhow::Result<Session> {
        let req = self.post([("method", "auth.getsession"), ("token", token)]);
        let GetSession { session } = req.send().await?.json().await?;
        Ok(session)
    }

    pub async fn scrobble(
        &mut self,
        artist: &str,
        track: &str,
        timestamp: DateTime<Utc>,
        album: Option<&str>,
        duration: Option<u64>,
    ) -> anyhow::Result<()> {
        let Some(session) = self.auth_session.as_deref() else {
            return Err(anyhow::Error::msg("not logged in"));
        };
        let req = self.post(
            [
                ("method", "track.scrobble"),
                ("artist[0]", artist),
                ("track[0]", track),
                ("timestamp[0]", &timestamp.timestamp().to_string()),
            ]
            .into_iter()
            .chain(Some("album[0]").zip(album))
            .chain(Some("duration[0]").zip(duration.map(|d| d.to_string()).as_deref()))
            .chain(Some(("sk", session))),
        );

        req.send().await?.error_for_status()?;
        Ok(())
    }

    pub async fn now_playing(
        &mut self,
        artist: &str,
        track: &str,
        album: Option<&str>,
        duration: Option<u64>,
    ) -> anyhow::Result<()> {
        let Some(session) = self.auth_session.as_deref() else {
            return Err(anyhow::Error::msg("not logged in"));
        };
        let req = self.post(
            [
                ("method", "track.updateNowPlaying"),
                ("artist", artist),
                ("track", track),
            ]
            .into_iter()
            .chain(Some("album").zip(album))
            .chain(Some("duration").zip(duration.map(|d| d.to_string()).as_deref()))
            .chain(Some(("sk", session))),
        );

        req.send().await?.error_for_status()?;
        Ok(())
    }
}
