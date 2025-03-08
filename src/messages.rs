/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
}

// SETUP
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub keepalive_timeout: u32,
    pub accept_keepalive_timeout: u32,
    pub version: String,
}

// KEEPALIVE
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepaliveMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
}

// AUTH
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub token: String,
}

// AUTH_STATE
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStateMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub state: String,
    pub user_id: Option<String>,
}

// CHANNEL_REQUEST
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRequestMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub service: String,
    pub parameters: HashMap<String, String>,
}

// CHANNEL_OPENED
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelOpenedMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub service: String,
    pub parameters: HashMap<String, String>,
}

// CHANNEL_CLOSED
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelClosedMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
}

// CHANNEL_CANCEL
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelCancelMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
}

// ERROR
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub error: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSubscription {
    #[serde(rename = "type")]
    pub event_type: String,
    pub symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

// FEED_SUBSCRIPTION
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSubscriptionMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<FeedSubscription>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<FeedSubscription>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<bool>,
}

// FEED_SETUP
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSetupMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub accept_aggregation_period: f64,
    pub accept_data_format: String,
    pub accept_event_fields: HashMap<String, Vec<String>>,
}

// FEED_CONFIG
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedConfigMessage {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub aggregation_period: f64,
    pub data_format: String,
    #[serde(default)]
    pub event_fields: Option<HashMap<String, Vec<String>>>,
}

// FEED_DATA
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedDataMessage<T> {
    pub channel: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub data: T,
}
