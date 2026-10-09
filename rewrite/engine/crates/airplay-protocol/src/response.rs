//! RTSP response building.

use super::RtspRequest;

/// An RTSP/HTTP response ready for the wire.
#[derive(Debug, Clone)]
pub struct RtspResponse {
    pub status: u16,
    pub reason: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RtspResponse {
    /// `200 OK` answering `req`, echoing its CSeq plus our Server header.
    pub fn ok(req: &RtspRequest) -> Self {
        let mut r = Self::status_only(200, "OK");
        r.echo_cseq(req);
        r.header("Server", "AirPlay/220.68.4");
        r
    }

    pub fn status_only(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn unauthorized(req: &RtspRequest) -> Self {
        let mut r = Self::status_only(401, "Unauthorized");
        r.echo_cseq(req);
        r
    }

    pub fn not_found(req: &RtspRequest) -> Self {
        let mut r = Self::status_only(404, "Not Found");
        r.echo_cseq(req);
        r
    }

    pub fn header(&mut self, name: &str, value: &str) -> &mut Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn session(&mut self, id: &str) -> &mut Self {
        self.header("Session", id)
    }

    pub fn body_bytes(&mut self, content_type: &str, body: Vec<u8>) -> &mut Self {
        self.header("Content-Type", content_type);
        self.header("Content-Length", &body.len().to_string());
        self.body = body;
        self
    }

    fn echo_cseq(&mut self, req: &RtspRequest) {
        if let Some(cseq) = req.header("cseq") {
            self.header("CSeq", cseq);
        }
    }

    /// Serialize to wire bytes (`RTSP/1.0 <status> <reason>\r\n...`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("RTSP/1.0 {} {}\r\n", self.status, self.reason).into_bytes();
        for (k, v) in &self.headers {
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse_request;
    use super::*;

    fn sample_request() -> RtspRequest {
        let raw = b"OPTIONS * RTSP/1.0\r\nCSeq: 7\r\n\r\n";
        parse_request(raw).unwrap().0
    }

    #[test]
    fn ok_response_serialization_is_byte_exact() {
        let req = sample_request();
        let resp = RtspResponse::ok(&req);
        assert_eq!(
            resp.to_bytes(),
            b"RTSP/1.0 200 OK\r\nCSeq: 7\r\nServer: AirPlay/220.68.4\r\n\r\n"
        );
    }

    #[test]
    fn response_with_plist_body() {
        let req = sample_request();
        let mut resp = RtspResponse::ok(&req);
        resp.session("ABCDEF01")
            .body_bytes("application/x-apple-binary-plist", vec![1, 2, 3]);
        let bytes = resp.to_bytes();
        assert!(bytes.windows(17).any(|w| w == b"Session: ABCDEF01"));
        assert!(bytes.ends_with(&[1, 2, 3]));
    }

    #[test]
    fn error_responses_echo_cseq() {
        let req = sample_request();
        assert!(
            String::from_utf8(RtspResponse::unauthorized(&req).to_bytes())
                .unwrap()
                .contains("401 Unauthorized")
        );
        assert!(String::from_utf8(RtspResponse::not_found(&req).to_bytes())
            .unwrap()
            .contains("CSeq: 7"));
    }
}
