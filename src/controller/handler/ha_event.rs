// Copyright (c) 2023 Unfolded Circle ApS, Markus Zehnder <markus.z@unfoldedcircle.com>
// SPDX-License-Identifier: MPL-2.0

//! Actix message handler for Home Assistant events.

use crate::client::messages::{AvailableEntities, EntityEvent};
use crate::controller::handler::{SubscribeHaEventsMsg, UnsubscribeHaEventsMsg};
use crate::controller::{Controller, OperationModeState, SendWsMessage};
use crate::errors::ServiceError;
use crate::util::DeserializeMsgData;
use actix::Handler;
use log::{debug, error};
use uc_api::intg::ws::AvailableEntitiesMsgData;
use uc_api::intg::{EntityChange, SubscribeEvents};
use uc_api::ws::{EventCategory, WsMessage};

impl Handler<EntityEvent> for Controller {
    type Result = ();

    fn handle(&mut self, msg: EntityEvent, _ctx: &mut Self::Context) -> Self::Result {
        // TODO keep an entity subscription per remote session and filter out non-subscribed remotes?
        if let Ok(msg_data) = serde_json::to_value(msg.entity_change) {
            for session in self.sessions.keys() {
                self.send_r2_msg(
                    WsMessage::event("entity_change", EventCategory::Entity, msg_data.clone()),
                    session,
                );
            }
        }
    }
}

impl Handler<AvailableEntities> for Controller {
    type Result = ();

    fn handle(&mut self, msg: AvailableEntities, _ctx: &mut Self::Context) -> Self::Result {
        // TODO just a quick implementation. Implement request filter! (also caching?)
        for (ws_id, session) in self.sessions.iter_mut() {
            if session.standby {
                debug!("[{ws_id}] Remote is in standby, not handling available_entities from HASS");
                continue;
            }
            if let Some(id) = session.get_available_entities_id {
                let msg_data = AvailableEntitiesMsgData {
                    filter: None,
                    available_entities: msg.entities.clone(),
                };
                if let Ok(msg_data_json) = serde_json::to_value(msg_data) {
                    match session
                        .recipient
                        .try_send(SendWsMessage(WsMessage::response(
                            id,
                            "available_entities",
                            msg_data_json.clone(),
                        ))) {
                        Ok(_) => session.get_available_entities_id = None,
                        Err(e) => error!("[{ws_id}] Error sending available_entities: {e:?}"),
                    }
                }
            } else if let Some(id) = session.get_entity_states_id {
                let mut msg_data = Vec::with_capacity(msg.entities.len());
                for entity in &msg.entities {
                    msg_data.push(EntityChange {
                        device_id: entity.device_id.clone(),
                        entity_type: entity.entity_type.clone(),
                        entity_id: entity.entity_id.clone(),
                        attributes: entity.attributes.clone().unwrap_or_default(),
                    });
                }

                if let Ok(msg_data_json) = serde_json::to_value(msg_data) {
                    match session
                        .recipient
                        .try_send(SendWsMessage(WsMessage::response(
                            id,
                            "entity_states",
                            msg_data_json.clone(),
                        ))) {
                        Ok(_) => session.get_entity_states_id = None,
                        Err(e) => error!("[{ws_id}] Error sending entity_states: {e:?}"),
                    }
                }
            }
        }
    }
}

impl Handler<SubscribeHaEventsMsg> for Controller {
    type Result = Result<(), ServiceError>;

    fn handle(&mut self, msg: SubscribeHaEventsMsg, _ctx: &mut Self::Context) -> Self::Result {
        if !matches!(self.machine.state(), &OperationModeState::Running) {
            return Err(ServiceError::ServiceUnavailable("Setup required".into()));
        }
        if let Some(session) = self.sessions.get_mut(&msg.0.ws_id) {
            let subscribe: SubscribeEvents = msg.0.deserialize()?;
            session
                .subscribed_entities
                .extend(subscribe.entity_ids.into_iter());
            Ok(())
        } else {
            Err(ServiceError::NotConnected)
        }
    }
}

impl Handler<UnsubscribeHaEventsMsg> for Controller {
    type Result = Result<(), ServiceError>;

    fn handle(&mut self, msg: UnsubscribeHaEventsMsg, _ctx: &mut Self::Context) -> Self::Result {
        if !matches!(self.machine.state(), &OperationModeState::Running) {
            return Err(ServiceError::ServiceUnavailable("Setup required".into()));
        }
        if let Some(session) = self.sessions.get_mut(&msg.0.ws_id) {
            let unsubscribe: SubscribeEvents = msg.0.deserialize()?;
            for i in unsubscribe.entity_ids {
                session.subscribed_entities.remove(&i);
            }
            Ok(())
        } else {
            Err(ServiceError::NotConnected)
        }
    }
}
