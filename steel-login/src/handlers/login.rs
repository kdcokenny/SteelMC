//! Login state packet handlers.

use rsa::Pkcs1v15Encrypt;
use sha1::Sha1;
use sha2::Digest;
use steel_core::{player::GameProfile, server::PlayerJoinReservationError};
use steel_protocol::{
    packets::login::{CHello, CLoginCompression, CLoginFinished, SHello, SKey},
    utils::ConnectionProtocol,
};
use steel_utils::translations;
use text_components::TextComponent;

use crate::{
    AuthError, is_valid_player_name, mojang_authenticate, offline_uuid, signed_bytes_be_to_hex,
    tcp_client::{ConnectionAction, ConnectionUpdate, JavaTcpClient},
};

impl JavaTcpClient {
    async fn reserve_player_join(&self, profile: &GameProfile) -> bool {
        match self
            .server
            .reserve_replacement_player_join(profile.id, &self.cancel_token)
            .await
        {
            Ok(reservation) => {
                if self.cancel_token.is_cancelled() {
                    return false;
                }

                let mut current = self.player_join_reservation.lock();
                if current.is_some() {
                    log::error!(
                        "Client {} attempted to reserve player admission twice",
                        self.id
                    );
                    drop(current);
                    self.close();
                    return false;
                }
                if self.cancel_token.is_cancelled() {
                    return false;
                }
                *current = Some(reservation);
                true
            }
            Err(PlayerJoinReservationError::Cancelled) => {
                self.close();
                false
            }
            Err(PlayerJoinReservationError::TimedOut) => {
                self.kick("Took too long to log in".into()).await;
                false
            }
        }
    }

    /// Handles the hello packet during the login state.
    pub(crate) async fn handle_hello(&self, packet: SHello) -> ConnectionAction {
        // The hello UUID is client supplied; only authentication or offline derivation is trusted.
        let requested_username = packet.name;
        if !is_valid_player_name(&requested_username) {
            self.kick("Invalid player name".into()).await;
            return ConnectionAction::none();
        }

        if self.server.config.encryption {
            let sequence_result = self.pre_play_state.lock().wait_for_key(requested_username);
            if let Err(error) = sequence_result {
                return self.reject_unexpected_packet(error).await;
            }

            let challenge: [u8; 4] = rand::random();
            self.challenge.store(challenge);

            self.send_bare_packet_now(CHello::new(
                String::new(),
                &self.server.key_store.public_key_der,
                challenge,
                true,
            ))
            .await;
            return ConnectionAction::none();
        }

        let profile = GameProfile {
            id: offline_uuid(&requested_username),
            name: requested_username,
            properties: vec![],
            profile_actions: None,
        };
        if !self.reserve_player_join(&profile).await {
            return ConnectionAction::none();
        }
        let action = self.send_login_finished(&profile).await;
        let sequence_result = self.pre_play_state.lock().complete_login(profile);
        if let Err(error) = sequence_result {
            return self.reject_unexpected_packet(error).await;
        }
        action
    }

    /// Handles the key packet during the login state, used for encryption.
    #[expect(
        clippy::too_many_lines,
        reason = "authentication, encryption negotiation, and profile reservation are one login transaction"
    )]
    pub(crate) async fn handle_key(&self, packet: SKey) -> ConnectionAction {
        let sequence_result = self.pre_play_state.lock().begin_authentication();
        let requested_username = match sequence_result {
            Ok(requested_username) => requested_username,
            Err(error) => return self.reject_unexpected_packet(error).await,
        };
        let challenge = self.challenge.load();

        let Ok(challenge_response) = self
            .server
            .key_store
            .private_key
            .decrypt(Pkcs1v15Encrypt, &packet.challenge)
        else {
            self.kick("Invalid key".into()).await;
            return ConnectionAction::none();
        };

        if challenge_response != challenge {
            self.kick("Invalid challenge response".into()).await;
            return ConnectionAction::none();
        }

        let Ok(secret_key) = self
            .server
            .key_store
            .private_key
            .decrypt(Pkcs1v15Encrypt, &packet.key)
        else {
            self.kick("Invalid key".into()).await;
            return ConnectionAction::none();
        };

        let secret_key: [u8; 16] = if let Ok(secret_key) = secret_key.try_into() {
            secret_key
        } else {
            self.kick("Invalid key".into()).await;
            return ConnectionAction::none();
        };

        let Ok(_) = self
            .connection_updates
            .send(ConnectionUpdate::EnableEncryption(secret_key))
        else {
            self.kick("Failed to send connection update".into()).await;
            return ConnectionAction::none();
        };

        tokio::select! {
            () = self.connection_updated.notified() => {}
            () = self.cancel_token.cancelled() => return ConnectionAction::none(),
        }

        let profile = if self.server.config.online_mode {
            let server_hash = &Sha1::new()
                .chain_update(secret_key)
                .chain_update(&self.server.key_store.public_key_der)
                .finalize();

            let server_hash = signed_bytes_be_to_hex(server_hash);

            match mojang_authenticate(
                &requested_username,
                &server_hash,
                self.server.config.auth_server.as_deref(),
            )
            .await
            {
                Ok(profile) => profile,
                Err(error) => {
                    self.kick(match error {
                        AuthError::FailedResponse => TextComponent::translated(
                            translations::MULTIPLAYER_DISCONNECT_AUTHSERVERS_DOWN.msg(),
                        ),
                        AuthError::UnverifiedUsername => TextComponent::translated(
                            translations::MULTIPLAYER_DISCONNECT_UNVERIFIED_USERNAME.msg(),
                        ),
                        AuthError::InvalidAuthServer(auth_server) => {
                            log::error!(
                                "Invalid authentication server URL configured: {auth_server}"
                            );
                            TextComponent::translated(
                                translations::MULTIPLAYER_DISCONNECT_AUTHSERVERS_DOWN.msg(),
                            )
                        }
                        e => e.to_string().into(),
                    })
                    .await;
                    return ConnectionAction::none();
                }
            }
        } else {
            GameProfile {
                id: offline_uuid(&requested_username),
                name: requested_username,
                properties: vec![],
                profile_actions: None,
            }
        };

        if !self.reserve_player_join(&profile).await {
            return ConnectionAction::none();
        }

        let action = self
            .send_login_finished(&profile)
            .await
            .with_reader_encryption(secret_key);
        let sequence_result = self.pre_play_state.lock().complete_login(profile);
        if let Err(error) = sequence_result {
            return self.reject_unexpected_packet(error).await;
        }
        action
    }

    /// Sends the successful login response.
    ///
    /// # Panics
    /// This function will panic if the compression threshold cannot be converted to an i32.
    pub(crate) async fn send_login_finished(&self, profile: &GameProfile) -> ConnectionAction {
        let mut action = ConnectionAction::none();
        if let Some(compression) = self.server.config.compression {
            self.send_bare_packet_now(CLoginCompression::new(
                compression
                    .threshold
                    .get()
                    .try_into()
                    .expect("Failed to convert compression threshold to i32"),
            ))
            .await;
            self.compression.store(Some(compression));
            action = ConnectionAction::reader_compression(compression);
        }

        self.send_bare_packet_now(CLoginFinished::new(
            profile.into(),
            self.connection_session.session_id(),
        ))
        .await;

        action
    }

    /// Handles the login acknowledged packet and transitions to the configuration state.
    pub(crate) async fn handle_login_acknowledged(&self) -> ConnectionAction {
        let sequence_result = self.pre_play_state.lock().acknowledge_login();
        if let Err(error) = sequence_result {
            return self.reject_unexpected_packet(error).await;
        }
        self.protocol.store(ConnectionProtocol::Config);

        self.start_configuration().await;
        ConnectionAction::none()
    }
}
