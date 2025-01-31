// Copyright (c) 2023 Unfolded Circle ApS, Markus Zehnder <markus.z@unfoldedcircle.com>
// SPDX-License-Identifier: MPL-2.0

//! Driver setup flow handling.

use crate::configuration::save_user_settings;
use crate::controller::handler::{AbortDriverSetup, SetDriverUserDataMsg, SetupDriverMsg};
use crate::controller::{Controller, OperationModeInput::*};
use crate::errors::{ServiceError, ServiceError::BadRequest};
use actix::{AsyncContext, Handler, Message};
use derive_more::Constructor;
use log::{debug, warn};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;
use uc_api::intg::{DriverSetupChange, IntegrationSetup};
use uc_api::model::intg::{IntegrationSetupError, IntegrationSetupState, SetupChangeEventType};
use uc_api::ws::{EventCategory, WsMessage};
use url::Url;

/// Local Actix message to request further user data.
#[derive(Constructor, Message)]
#[rtype(result = "()")]
struct RequestExpertOptionsMsg {
    pub ws_id: String,
}

/// Local Actix message to finish setup flow.
#[derive(Constructor, Message)]
#[rtype(result = "()")]
struct FinishSetupFlowMsg {
    pub ws_id: String,
    pub error: Option<IntegrationSetupError>,
}

impl Handler<SetupDriverMsg> for Controller {
    type Result = Result<(), ServiceError>;

    fn handle(&mut self, msg: SetupDriverMsg, ctx: &mut Self::Context) -> Self::Result {
        debug!("[{}] {:?}", msg.ws_id, msg.data);

        if self
            .sm_consume(&msg.ws_id, &SetupDriverRequest, ctx)
            .is_err()
        {
            return Err(BadRequest(
                "Cannot start driver setup. Please abort setup first.".into(),
            ));
        }

        let mut cfg = self.settings.hass.clone();

        // validate setup data
        cfg.url = validate_url(msg.data.setup_data.get("url").map(|u| u.as_str()))?;

        if let Some(token) = msg.data.setup_data.get("token") {
            if token.trim().is_empty() {
                warn!(
                    "[{}] no token value provided in setup, using existing token",
                    msg.ws_id
                )
            } else {
                cfg.token = token.clone();
            }
        } else {
            return Err(BadRequest("Missing field: token".into()));
        }

        save_user_settings(&cfg)?;

        // TODO verify WebSocket connection to make sure user provided URL & taken are ok! #3
        // Right now the core will just send a Connect request after setup...
        self.settings.hass = cfg;

        // use a delay that the ack response will be sent first
        let delay = Duration::from_millis(100);
        if msg
            .data
            .setup_data
            .get("expert")
            .and_then(|v| bool::from_str(v).ok())
            .unwrap_or_default()
        {
            // start expert setup with an additional configuration screen
            ctx.notify_later(RequestExpertOptionsMsg::new(msg.ws_id), delay);
        } else {
            // setup done!
            ctx.notify_later(FinishSetupFlowMsg::new(msg.ws_id, None), delay);
        }

        // this will acknowledge the setup_driver request message
        Ok(())
    }
}

impl Handler<SetDriverUserDataMsg> for Controller {
    type Result = Result<(), ServiceError>;

    fn handle(&mut self, msg: SetDriverUserDataMsg, ctx: &mut Self::Context) -> Self::Result {
        debug!("[{}] {:?}", msg.ws_id, msg.data);

        if self.sm_consume(&msg.ws_id, &SetupUserData, ctx).is_err() {
            return Err(BadRequest(
                "Not waiting for driver user data. Please restart setup.".into(),
            ));
        }

        // validate setup data
        let mut cfg = self.settings.hass.clone();
        if let IntegrationSetup::InputValues(values) = msg.data {
            if let Some(value) = parse_value(&values, "connection_timeout") {
                if value >= 3 {
                    cfg.connection_timeout = value;
                }
            }
            if let Some(value) = parse_value(&values, "max_frame_size_kb") {
                if value >= 1024 {
                    cfg.max_frame_size_kb = value;
                }
            }
            if let Some(value) = parse_value(&values, "heartbeat_interval") {
                if value >= 3 {
                    cfg.heartbeat.interval = Duration::from_secs(value);
                }
            }
            if let Some(value) = parse_value(&values, "heartbeat_timeout") {
                if value >= 6 {
                    cfg.heartbeat.timeout = Duration::from_secs(value);
                }
            }
            if let Some(value) = parse_value(&values, "reconnect.attempts") {
                cfg.reconnect.attempts = value;
            }
            if let Some(value) = parse_value(&values, "reconnect.duration_ms") {
                cfg.reconnect.duration = Duration::from_millis(value);
            }
            if let Some(value) = parse_value(&values, "reconnect.duration_max_ms") {
                cfg.reconnect.duration_max = Duration::from_millis(value);
            }
            if let Some(value) = parse_value(&values, "reconnect.backoff_factor") {
                if value >= 1f32 {
                    cfg.reconnect.backoff_factor = value;
                }
            }
        } else {
            return Err(BadRequest("Invalid response: require input_values".into()));
        }

        save_user_settings(&cfg)?;
        self.settings.hass = cfg;

        // use a delay that the ack response will be sent first
        ctx.notify_later(
            FinishSetupFlowMsg::new(msg.ws_id, None),
            Duration::from_millis(100),
        );

        // this will acknowledge the set_driver_user_data request message
        Ok(())
    }
}

impl Handler<RequestExpertOptionsMsg> for Controller {
    type Result = ();

    fn handle(&mut self, msg: RequestExpertOptionsMsg, ctx: &mut Self::Context) -> Self::Result {
        if self.sm_consume(&msg.ws_id, &RequestUserInput, ctx).is_err() {
            return;
        }

        let event = WsMessage::event(
            "driver_setup_change",
            EventCategory::Device,
            json!({
                "event_type": SetupChangeEventType::Setup,
                "state": IntegrationSetupState::WaitUserAction,
                "require_user_action": {
                    "input": {
                        "title": {
                            "en": "Expert configuration"
                        },
                        "settings": [
                            {
                                "id": "connection_timeout",
                                "label": {
                                    "en": "Connection timeout in seconds"
                                },
                                "field": {
                                    "number": {
                                        "value": self.settings.hass.connection_timeout,
                                        "min": 3,
                                        "max": 30,
                                        "unit": { "en": "sec" } // not yet working in web-configurator
                                    }
                                }
                            },
                            {
                                "id": "max_frame_size_kb",
                                "label": {
                                    "en": "Max WebSocket frame size (kilobyte)"
                                },
                                "field": {
                                    "number": {
                                        "value": self.settings.hass.max_frame_size_kb,
                                        "min": 1024,
                                        "max": 16384,
                                        "unit": { "en": "KB" }
                                    }
                                }
                            },
                            {
                                "id": "reconnect.attempts",
                                "label": {
                                    "en": "Max reconnect attempts"
                                },
                                "field": {
                                    "number": {
                                        "value": self.settings.hass.reconnect.attempts,
                                        "min": 1,
                                        "max": 65536
                                    }
                                }
                            },
                            {
                                "id": "reconnect.duration_ms",
                                "label": {
                                    "en": "Initial reconnect delay in milliseconds"
                                },
                                "field": {
                                    "number": {
                                        "value": self.settings.hass.reconnect.duration.as_millis(),
                                        "min": 100,
                                        "max": 600000,
                                        "unit": { "en": "ms" }
                                    }
                                }
                            },
                            {
                                "id": "reconnect.duration_max_ms",
                                "label": {
                                    "en": "Max reconnect delay in milliseconds"
                                },
                                "field": {
                                    "number": {
                                        "value": self.settings.hass.reconnect.duration_max.as_millis(),
                                        "min": 1000,
                                        "max": 600000,
                                        "unit": { "en": "ms" }
                                    }
                                }
                            },
                            {
                                "id": "reconnect.backoff_factor",
                                "label": {
                                    "en": "Reconnect backoff factor"
                                },
                                "field": {
                                    "number": {
                                        "value": self.settings.hass.reconnect.backoff_factor,
                                        "min": 1,
                                        "max": 10,
                                        "decimals": 1,
                                    }
                                }
                            },
                            {
                                "id": "heartbeat_interval",
                                "label": {
                                    "en": "Heartbeat interval in seconds"
                                },
                                "field": {
                                    "number": {
                                        "value": self.settings.hass.heartbeat.interval.as_secs(),
                                        "min": 3,
                                        "max": 60,
                                        "unit": { "en": "sec" }
                                    }
                                }
                            },
                            {
                                "id": "heartbeat_timeout",
                                "label": {
                                    "en": "Heartbeat timeout in seconds"
                                },
                                "field": {
                                    "number": {
                                        "value": self.settings.hass.heartbeat.timeout.as_secs(),
                                        "min": 6,
                                        "max": 300,
                                        "unit": { "en": "sec" }
                                    }
                                }
                            }
                        ]
                    }
                }
            }),
        );
        self.send_r2_msg(event, &msg.ws_id);
    }
}

impl Handler<FinishSetupFlowMsg> for Controller {
    type Result = ();

    fn handle(&mut self, msg: FinishSetupFlowMsg, ctx: &mut Self::Context) -> Self::Result {
        let input = if msg.error.is_none() {
            Successful
        } else {
            SetupError
        };
        if self.sm_consume(&msg.ws_id, &input, ctx).is_err() {
            return;
        }

        let event = WsMessage::event(
            "driver_setup_change",
            EventCategory::Device,
            serde_json::to_value(DriverSetupChange {
                event_type: SetupChangeEventType::Stop,
                state: if msg.error.is_none() {
                    IntegrationSetupState::Ok
                } else {
                    IntegrationSetupState::Error
                },
                error: msg.error,
                require_user_action: None,
            })
            .expect("DriverSetupChange serialize error"),
        );
        self.send_r2_msg(event, &msg.ws_id);
    }
}

impl Handler<AbortDriverSetup> for Controller {
    type Result = ();

    fn handle(&mut self, msg: AbortDriverSetup, ctx: &mut Self::Context) -> Self::Result {
        debug!(
            "[{}] abort driver setup request, timeout: {}",
            msg.ws_id, msg.timeout
        );

        if msg.timeout {
            if self.sm_consume(&msg.ws_id, &SetupError, ctx).is_err() {
                return;
            }
            // notify Remote Two that we ran into a timeout
            ctx.notify(FinishSetupFlowMsg {
                ws_id: msg.ws_id,
                error: Some(IntegrationSetupError::Timeout),
            })
        } else {
            // abort: Remote Two aborted setup flow
            if self.sm_consume(&msg.ws_id, &AbortSetup, ctx).is_err() {
                return;
            }
        }

        if let Some(handle) = self.setup_timeout.take() {
            ctx.cancel_future(handle);
        }

        // Note: this is the place to cleanup any setup activities
        // e.g. stopping the planned Home Assistant mDNS server discovery etc
        // For now it's just a state transition
    }
}

fn parse_value<T: FromStr>(map: &HashMap<String, String>, key: &str) -> Option<T> {
    map.get(key).and_then(|v| T::from_str(v).ok())
}

/// Validate and convert Home Assistant WebSocket URL
fn validate_url<'a>(addr: impl Into<Option<&'a str>>) -> Result<Url, ServiceError> {
    let addr = match addr.into() {
        None => return Err(BadRequest("Missing field: url".into())),
        Some(addr) => addr.trim(),
    };

    // user provided URL might missing scheme
    let mut url = match Url::parse(addr) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) => parse_with_ws_scheme(addr)?,
        Err(e) => {
            warn!("Invalid WebSocket URL '{addr}': {e}");
            return Err(e.into());
        }
    };

    // quirk of URL parsing: hostname:port detects the hostname as scheme!
    if url.host_str().is_none() {
        url = parse_with_ws_scheme(addr)?;
    }

    match url.scheme() {
        "http" => {
            let _ = url.set_scheme("ws");
        }
        "https" => {
            let _ = url.set_scheme("wss");
        }
        "ws" | "wss" => { /* ok */ }
        _ => {
            return Err(BadRequest(
                "Invalid scheme, allowed: ws, wss, http, https".into(),
            ))
        }
    }

    Ok(url)
}

fn parse_with_ws_scheme(address: &str) -> Result<Url, url::ParseError> {
    let address = format!("ws://{address}");
    Url::parse(&address).map_err(|e| {
        warn!("Invalid URL '{address}': {e}");
        e
    })
}

#[cfg(test)]
mod tests {
    use super::validate_url;
    use crate::errors::{ServiceError, ServiceError::BadRequest};
    use url::Url;

    fn url(url: &str) -> Result<Url, ServiceError> {
        match Url::parse(url) {
            Ok(url) => Ok(url),
            Err(e) => panic!("valid URL required! {e}"),
        }
    }

    #[test]
    fn empty_address_returns_error() {
        let result = validate_url(None);
        assert!(matches!(result, Err(BadRequest(_))));
        let result = validate_url("");
        assert!(matches!(result, Err(BadRequest(_))));
        let result = validate_url("  ");
        assert!(matches!(result, Err(BadRequest(_))));
    }

    #[test]
    fn host_only() {
        assert_eq!(url("ws://test/"), validate_url("test"));
    }

    #[test]
    fn valid_address_returns_url() {
        assert_eq!(
            url("ws://homeassistant.local:8123/api/websocket"),
            validate_url("ws://homeassistant.local:8123/api/websocket")
        );
    }

    #[test]
    fn address_with_spaces_are_trimmed() {
        assert_eq!(url("ws://test/"), validate_url("  test   "));
        assert_eq!(
            url("ws://homeassistant.local:8123/api/websocket"),
            validate_url("  ws://homeassistant.local:8123/api/websocket   ")
        );
    }

    #[test]
    fn host_only_with_port() {
        assert_eq!(url("ws://test:8123/"), validate_url("test:8123"));
    }

    #[test]
    fn ip_address_only() {
        assert_eq!(url("ws://127.0.0.1/"), validate_url("127.0.0.1"));
    }

    #[test]
    fn ip_address_only_with_port() {
        assert_eq!(url("ws://127.0.0.1:123/"), validate_url("127.0.0.1:123"));
    }

    #[test]
    fn add_scheme_if_missing() {
        assert_eq!(url("ws://test:123/foo"), validate_url("test:123/foo"));
    }

    #[test]
    fn force_ws_scheme_from_http() {
        assert_eq!(url("ws://test/"), validate_url("http://test"));
        assert_eq!(url("wss://test/"), validate_url("https://test"));
        assert_eq!(url("ws://test/"), validate_url("HTTP://test"));
        assert_eq!(url("wss://test/"), validate_url("HTTPS://test"));
    }

    #[test]
    fn invalid_scheme_returns_error() {
        let result = validate_url("foo://test");
        assert!(matches!(result, Err(BadRequest(_))));
    }
}
