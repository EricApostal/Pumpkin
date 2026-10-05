//! The encoding flavour of a Java connection.
//!
//! A client of a mod loader that changed the network encoding of some play packets (NeoForge
//! does, see `docs/neoforge-play-codecs.md` of the plugin API) has to get those packets written
//! its way, while a vanilla client must not. The flavour of a connection is chosen by a plugin
//! during the configuration phase and is read by the packet writers through
//! [`ConnectionFlavour::current`].
//!
//! # Why a thread local
//!
//! [`ClientPacket::write_packet_data`](crate::ClientPacket::write_packet_data) is synchronous and
//! has hundreds of implementations, of which only a handful depend on the flavour. Adding a
//! parameter would touch all of them and every call site. Instead the code that serializes a
//! packet for one connection wraps the call in [`ConnectionFlavour::scope`], and the few packets
//! that differ read the active flavour. Nothing can interleave inside the scope (it holds no
//! `.await`), the previous value is restored on exit (also when a writer panics), and code that
//! does not set a scope gets [`ConnectionFlavour::Vanilla`], which is the unchanged behaviour.

/// How the packets of a connection are encoded beyond the vanilla protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ConnectionFlavour {
    /// The vanilla encoding.
    #[default]
    Vanilla,
    /// The client was told it talks to a NeoForge server (it received `neoforge:register` before
    /// the brand), so it reads NeoForge's variants of some packets.
    NeoForge,
}

thread_local! {
    /// The flavour of the packet being serialized on this thread.
    static ACTIVE: std::cell::Cell<ConnectionFlavour> =
        const { std::cell::Cell::new(ConnectionFlavour::Vanilla) };
}

/// Puts the previous flavour back, also when the scope unwinds.
struct Restore(ConnectionFlavour);

impl Drop for Restore {
    fn drop(&mut self) {
        ACTIVE.set(self.0);
    }
}

impl ConnectionFlavour {
    #[must_use]
    pub const fn is_neoforge(self) -> bool {
        matches!(self, Self::NeoForge)
    }

    /// The flavour packets are being written for on this thread, [`Vanilla`](Self::Vanilla)
    /// outside of a [`scope`](Self::scope).
    #[must_use]
    pub fn current() -> Self {
        ACTIVE.get()
    }

    /// Runs `write` with `self` as the active flavour.
    pub fn scope<R>(self, write: impl FnOnce() -> R) -> R {
        let _restore = Restore(ACTIVE.replace(self));
        write()
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionFlavour;

    #[test]
    fn defaults_to_vanilla_outside_a_scope() {
        assert_eq!(ConnectionFlavour::current(), ConnectionFlavour::Vanilla);
    }

    #[test]
    fn scope_sets_and_restores_the_flavour() {
        let seen = ConnectionFlavour::NeoForge.scope(|| {
            let outer = ConnectionFlavour::current();
            let inner = ConnectionFlavour::Vanilla.scope(ConnectionFlavour::current);
            (outer, inner, ConnectionFlavour::current())
        });
        assert_eq!(
            seen,
            (
                ConnectionFlavour::NeoForge,
                ConnectionFlavour::Vanilla,
                ConnectionFlavour::NeoForge
            )
        );
        assert_eq!(ConnectionFlavour::current(), ConnectionFlavour::Vanilla);
    }

    #[test]
    fn scope_restores_after_a_panic() {
        let result = std::panic::catch_unwind(|| {
            ConnectionFlavour::NeoForge.scope(|| {
                panic!("writer failed");
            });
        });
        assert!(result.is_err());
        assert_eq!(ConnectionFlavour::current(), ConnectionFlavour::Vanilla);
    }
}
