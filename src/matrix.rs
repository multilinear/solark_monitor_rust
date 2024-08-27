use serde::Deserialize;

type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

//***************************** Matrix **********************************
#[derive(Debug, Deserialize, Clone)]
pub struct MatrixSettings {
    enable: bool,
    user: String,
    passwd: String,
    server: String,
    allowed_users: Vec<String>,
    pub room_check_secs: u64,
}

pub struct Matrix {
    cfg: MatrixSettings,
    allowed_users: Vec<String>,
    sync_settings: matrix_sdk::config::SyncSettings,
    client: Option<matrix_sdk::Client>, 
}
impl Matrix {
    pub fn new(cfg: &MatrixSettings) -> Self {
        use matrix_sdk::config::SyncSettings;
        let mut allowed_users = cfg.allowed_users.clone();
        allowed_users.push(cfg.user.clone());
        Matrix{
            cfg: cfg.clone(),
            allowed_users: allowed_users,
            sync_settings: SyncSettings::default(),
            client: None,
        }
    }
    pub async fn check_rooms(&mut self) -> Result<()> {
        if self.client.is_none() { return Ok(()); }
        if !self.cfg.enable { return Ok(()); }
        let client = self.client.as_ref().unwrap();
        client.sync_once(self.sync_settings.clone()).await?;
        let mut sync_again = false;
        for room in client.joined_rooms() {
            let ids = room.joined_user_ids().await?;
            if !ids.iter().any(|x| x.as_str() != self.cfg.user) {
                println!("Matrix room {0:?} is empty, leaving", room.room_id());
                room.leave().await?;
                sync_again = true;
            }
            for id in ids {
                if !self.allowed_users.iter().any(|x| x.as_str()==id) {
                    println!("User {id:?} is in the room, leaving");
                    room.leave().await?;
                    sync_again = true;
                }
            }
        }
        for room in client.invited_rooms() {
            let invite = room.invite_details().await?;
            let id = invite.inviter.as_ref().unwrap().user_id();
            if self.allowed_users.iter().any(|x| x.as_str()==id) {
                println!("User {id:?} invited us to a {0:?}, joining", room.room_id());
                if client.join_room_by_id(room.room_id()).await.is_err() {
                    // If joining doesn't work, leave the room so it drops
                    // off the invite list
                    println!("Room join failed, leaving instead");
                    room.leave().await?;
                }
                sync_again = true;
            } else {
                println!("SECURITY WARNING! {0:?} invited me to chat", id);
                room.leave().await?;
                sync_again = true;
            }
        }
        if sync_again {
            client.sync_once(self.sync_settings.clone()).await?;
        }
        Ok(())
    }
    pub async fn connect(&mut self) -> Result<()> {
        if self.client.is_some() { return Ok(()); }
        if !self.cfg.enable { return Ok(()); }
        use matrix_sdk::Client;
        let client = Client::builder().homeserver_url(&self.cfg.server).build().await?;
        client.matrix_auth().login_username(&self.cfg.user, &self.cfg.passwd).send().await?;
        println!("connected to Matrix {0:?}", self.cfg);
        self.client = Some(client);
        self.check_rooms().await?;
        Ok(())
    }
    /*async fn sync(&mut self) -> Result<()> {
        if !self.cfg.enable { return Ok(()); }
        self.connect().await?;
        let client = self.client.as_mut().unwrap();
        client.sync_once(self.sync_settings.clone()).await?;
        Ok(())
    }*/
    pub async fn disconnect(&mut self) -> Result<()> {
        self.client = None;
        Ok(())
    }
    pub async fn send(&mut self, msg: &str) -> Result<()> {
        use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
        if !self.cfg.enable { return Ok(()); }
        println!("Sending msg {msg:?} via matrix");
        self.connect().await?;
        let client = self.client.as_mut().unwrap();
        let rooms = client.joined_rooms();
        println!("Sending msg {msg:?} via matrix to {0:?} rooms", rooms.len());
        for room in rooms {
            let matrix_msg = RoomMessageEventContent::text_plain(msg);
            room.send(matrix_msg).await?;
        }
        Ok(())
    }
}

