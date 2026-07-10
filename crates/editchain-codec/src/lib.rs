#![cfg_attr(not(feature = "use-std"), no_std)]
#![doc = "Editchain binary codec — no_std frame encoding via postcard."]

pub mod frame;
pub mod page;