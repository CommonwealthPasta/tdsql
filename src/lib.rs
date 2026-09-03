#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;

mod client;
mod command;
mod config;
mod dataset;
mod error;
mod row;
mod transaction;
mod value;

pub use client::Client;
pub use command::{Command, CommandType, Parameter};
pub use config::Config;
pub use dataset::{DataSet, DataTable};
pub use error::{Error, Result};
pub use row::{Column, Row, RowIndex};
pub use transaction::{IsolationLevel, Transaction};
pub use value::{DataValue, FromSql, SqlType, ToSql};
