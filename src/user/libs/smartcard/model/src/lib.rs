//! THE SMART-CARD MODEL SHARED BY EVERY SIDE OF A READER: the answer-to-reset a card gives when it is
//! powered, parsed and validated once.
//!
//! A reader provider parses the ATR to negotiate with the card; SmartcardService validates what the
//! provider reports and takes the protocol from it; the in-guest fixture checks the ATR it plays is one
//! a real parser accepts. None of them has a parser of its own.

#![cfg_attr(not(test), no_std)]

pub mod atr;
