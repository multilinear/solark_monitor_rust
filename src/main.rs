use tokio;
use serde::Deserialize;
use std::time::{SystemTime, Duration};
use std::collections::BTreeMap;

type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

//**************************** Solark (modbus) *******************************

#[derive(Debug)]
enum RegData {
    Error,
    Bool(bool),
    Watts(i16),
    Percent(u16),
    Hz(u16),
    Volts(i16),
}

// We store the actual data in a vector
// This way we can cache the index and avoid doing
// tree lookups every cycle
type SolarkData = Vec<RegData>;
// Seperately we store an index into that vector
type SolarkIndex = BTree<String, i64>;


#[derive(Debug, Deserialize, Clone)]
enum RegDataType {
    Bool,
    Bool64Bit, // Used to read the fault code and see if it's nonzero
    Watts,
    Percent,
    Hz,
    Volts,
}

#[derive(Debug, Deserialize, Clone)]
struct SolarkReg {
  metric: String, // What to call the data
  addr: u16, // Address of the register
  datatype: RegDataType, // How to store the final data
}

#[derive(Debug, Deserialize, Clone)]
struct SolarkSettings {
  poll_secs: u32,
  port: String,
  slave_id: u8,
  registers: Vec<SolarkReg>,
}

#[derive(Debug)]
struct Solark {
  cfg: SolarkSettings,
  serial: Option<String>,
	ctx: Option<tokio_modbus::client::Context>,
}	

impl Solark {
  fn new(cfg: &SolarkSettings) -> Self {
      Self{
        cfg: cfg.clone(),
        serial: None,
        ctx: None,
      }
  }

  pub async fn connect(&mut self) -> Result<()> {
    if self.ctx.is_some() { return Ok(()); }
    use tokio_modbus::prelude::*;
    // connect
    let slave = Slave(self.cfg.slave_id); // Solark modbus ID

    let builder = tokio_serial::new(&self.cfg.port, 9600);
    let port = tokio_serial::SerialStream::open(&builder)?;

    let mut ctx = rtu::attach_slave(port, slave);
    // check the connection by pulling the serial number
    let rsp = ctx.read_holding_registers(3, 5).await??;
    let mut serial = String::with_capacity(10);
    for nibble in rsp {
        // as u8 takes the bottom byte
        serial.push((nibble>>8) as u8 as char); 
        serial.push(nibble as u8 as char); 
    }
    println!("connected to Solark Serial# {0:?} on port {1:?}", serial, self.cfg.port);
    self.ctx = Some(ctx);
    self.serial = Some(serial);
    return Ok(());
	}

  fn make_signed(v: u16) -> i16 {
    // massage into a signed value
    if v > (1<<(16-1))-1 {
        (v as i32 - (1<<16)) as i16
    } else {
        v as i16
    }
  }

	async fn read_register(ctx: &mut tokio_modbus::client::Context, reg: &SolarkReg) -> Result<RegData> {
    use tokio_modbus::prelude::*;
    let count = match reg.datatype {
            RegDataType::Bool64Bit => 4,
            _ => 1,
        };
    let v = ctx.read_holding_registers(reg.addr, count).await??;
    Ok(match reg.datatype {
        RegDataType::Bool64Bit => RegData::Bool(v[0] | v[1] | v[2] | v[3] != 0),
        RegDataType::Bool => RegData::Bool(v[0] != 0),
        RegDataType::Watts => RegData::Watts(Self::make_signed(v[0])),
        RegDataType::Percent => RegData::Percent(v[0]),
        RegDataType::Hz => RegData::Hz(v[0]),
        RegDataType::Volts => RegData::Volts(Self::make_signed(v[0])),
     })
	}

  pub fn new_data() -> SolarkData {
    let mut results = SolarkData::new();
    for reg in &self.cfg.registers {
        results.insert(reg.metric.clone(), Rc::new(RegData::Error));
    }
    return results;
  }

  pub async fn read_all(&mut self, &mut data: SolarkData) -> () {
    self.connect().await?;
    // connect succeeded so ctx exists
    let ctx = &mut self.ctx.as_mut().unwrap();
    // Sadly we can't parallelize this because self has to be borrowed as mutable
    // It's probably good though, since that would be a threadsafety problem
    // We can still do non-modbus stuff, like talk to influxdb, while awaiting
    for reg in &self.cfg.registers {
        let data = Solark::read_register(ctx, &reg).await?;
        results.get(reg.metric, data);
    }
    Ok(results)
  }

	async fn disconnect(&mut self) -> Result<()> {
    match &mut self.ctx {
      Some(ctx) => {
	        println!("Disconnecting");
          ctx.disconnect().await??;
        },
      None => (),
    }
    Ok(())
	}
}

//***************************** Influx **********************************

// This is a workaround.
// ideally we'd just use a BTreeMap<String,String>, but
// the config crate case squashes keys. Doing it this way
// makes the tag a value, rather than a key.
#[derive(Debug, Deserialize, Clone)]
struct SettingsPair {
    key: String,
    val: String,
}

#[derive(Debug, Deserialize, Clone)]
struct InfluxSettings {
    enable: bool,
    token: String,
    bucket: String,
    org: String,
    url: String,
    tags: Vec<SettingsPair>,
}

struct Influx {
    cfg: InfluxSettings,
    client: Option<influxdb2::Client>,
}

impl Influx {
    fn new(cfg: &InfluxSettings) -> Self {
        Self{
            cfg: cfg.clone(),
            client: None,
        }
    }
    async fn connect(&mut self) -> Result<()> {
      if !self.cfg.enable { return Ok(()); }
      match self.client {
        Some(_) => Ok(()),
        None =>  {
            use influxdb2::Client;
            let cfg = &self.cfg;
            let client = Client::new(&cfg.url, &cfg.org, &cfg.token);
            println!("connected to InfluxDB at {0:?}", self.cfg.url);
            self.client = Some(client);
            Ok(())
          },
      }
    }
    async fn disconnect(&mut self) -> Result<()> {
        self.client = None;
        Ok(())
    }
    async fn write_point(&mut self, data: &SolarkData) -> Result<()> {
        if !self.cfg.enable { return Ok(()); }
        // Automatically reconnect if we're not connected
        self.connect().await?;
        // connect either created client, or errored out
        // so unwrap can't fail here
        let client = self.client.as_mut().unwrap();
        // Build up the list of points
        use influxdb2::models::DataPoint;
        let timestamp = chrono::Utc::now().timestamp_nanos_opt().unwrap();
        let cfg = &self.cfg;
        // rust thinks cfg could escape scope, so we have to collect
        // the iterator rather than lazily evaluating.
        let points: Vec<DataPoint> =  data.iter().map(|(metric, datum)| {
            let point = DataPoint::builder(metric);
            cfg.tags.iter().fold(
                match datum {
                    RegData::Bool(v) => 
                        point.tag("units", "Bool")
                        .field("value", if *v {1} else {0}),
                    RegData::Watts(v) =>
                        point.tag("units", "Watts")
                        .field("value", *v as i64),
                    RegData::Percent(v) =>
                        point.tag("units", "Percent")
                        .field("value", *v as i64),
                    RegData::Hz(v) =>
                        point.tag("units", "Hz")
                        .field("value", *v as i64),
                    RegData::Volts(v) =>
                        point.tag("units", "Volts")
                        .field("value", *v as f64 / 10.0),
                }.timestamp(timestamp),
                |p, tag| p.tag(&tag.key, &tag.val)
            ).build().unwrap()
        }).collect();
        client.write(&self.cfg.bucket, futures::stream::iter(points)).await?;
        Ok(())
    }
}

//***************************** Matrix **********************************
#[derive(Debug, Deserialize, Clone)]
struct MatrixSettings {
    enable: bool,
    user: String,
    passwd: String,
    server: String,
    allowed_users: Vec<String>,
    room_check_cycles: i32,
}

struct Matrix {
    cfg: MatrixSettings,
    allowed_users: Vec<String>,
    sync_settings: matrix_sdk::config::SyncSettings,
    client: Option<matrix_sdk::Client>, 
}
impl Matrix {
    fn new(cfg: &MatrixSettings) -> Self {
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
    async fn check_rooms(&mut self) -> Result<()> {
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
    async fn connect(&mut self) -> Result<()> {
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
    async fn disconnect(&mut self) -> Result<()> {
        self.client = None;
        Ok(())
    }
    async fn send(&mut self, msg: &str) -> Result<()> {
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
    // TODO: we need stuff for accepting chat requests
}

//***************************** Alerts **********************************
#[derive(Debug, Deserialize, Clone)]
struct AlertDesc {
    metric: String,
    check: String,
    limit: f64,
    msg: String, 
    delaysecs: u64,
}

#[derive(Debug, Deserialize, Clone)]
struct AlertSettings {
    enable: bool,
    alerts: Vec<AlertDesc>,
    alert_timeout_secs: u32,
}

#[derive(Debug, Clone)]
enum AlertState {
    Idle,
    Pending(SystemTime),
    Firing(SystemTime),
}

struct Alert {
    metric: String,
    fun: Box<dyn Fn(f64) -> bool>,
    state: AlertState,
    msg: String,
    delay: Duration,
}

struct Alerting {
    enable: bool,
    alerter: Matrix,
    alerts: Vec<Alert>,
    msgs: BTreeMap<String, SystemTime>,
    alert_timeout: Duration,
}

impl Alerting {
    fn new(cfg: &AlertSettings, alerter: Matrix) -> Result<Self> {
        let mut alerts = Vec::with_capacity(cfg.alerts.len());
        for a in cfg.alerts.iter() {
            let val : f64 = a.limit;
            alerts.push(
                Alert {
                    metric: a.metric.clone(),
                    fun: if a.check.starts_with(">=") {
                            Box::new(move |x| x >= val)
                        } else if a.check.starts_with("<=") {
                            Box::new(move |x| x <= val)
                        } else if a.check.starts_with("=") {
                            Box::new(move |x| x == val)
                        } else if a.check.starts_with("!=") {
                            Box::new(move |x| x != val)
                        } else if a.check.starts_with(">") {
                            Box::new(move |x| x > val)
                        } else if a.check.starts_with("<") {
                            Box::new(move |x| x < val)
                        } else {
                            std::panic!("Failed to parse alert check expression from config");
                        },
                    state: AlertState::Idle,
                    msg: a.msg.clone(),
                    delay: Duration::from_secs(a.delaysecs),
                }
            )
        }
        Ok(Self {
            enable: cfg.enable,
            alerter: alerter,
            alerts: alerts,
            alert_timeout: Duration::from_secs(cfg.alert_timeout_secs.into()),
            msgs: BTreeMap::new(),
        })
    }
    async fn check(&mut self, data: &SolarkData) -> Result<()> {
        if !self.enable { return Ok(()); }
        let tn = SystemTime::now();
        // We're effectively implementing a very very shitty little
        // DSL here. Possibly we could plug in a rust crate that includes
        // an interpreter for something more real?
        for i in 0..self.alerts.len() {
            let a = &mut self.alerts[i];
            // This is a misconfiguration, so we want to crash
            // note that this will happen on the first run of "check"
            // so it won't surprise us when an alert fires or something
            let val = match data.get(&a.metric).expect(
                    &format!("Alert metric {0:?} not found in monitored data, fix your config", a.metric)
                  ) {
                RegData::Watts(v) => *v as f64,
                RegData::Percent(v) => *v as f64,
                RegData::Hz(v) => *v as f64,
                RegData::Volts(v) => *v as f64,
                RegData::Bool(v) => *v as i64 as f64,
            };
            match ((a.fun)(val), &a.state) {
                (true, AlertState::Firing(t)) => 
                    if tn > *t + self.alert_timeout {
                        a.state = AlertState::Firing(tn);
                        let msg = format!("Realert: {0:?}, {1:?}={val:?}", a.msg, a.metric);
                        Self::alert(&mut self.alerter, &msg).await?
                    },
                (true, AlertState::Pending(t)) => 
                    if tn > *t + a.delay {
                        a.state = AlertState::Firing(tn);
                        let msg = format!("Alert: {0:?}, {1:?}={val:?}", a.msg, a.metric);
                        Self::alert(&mut self.alerter, &msg).await?
                    },
                (true, AlertState::Idle) => {
                        if a.delay == Duration::ZERO {
                            a.state = AlertState::Firing(tn);
                            let msg = format!("Alert: {0:?}, {1:?}={val:?}", a.msg, a.metric);
                            Self::alert(&mut self.alerter, &msg).await?
                        } else {
                            a.state = AlertState::Pending(tn);
                        };
                    },
                (false, AlertState::Firing(_)) => {
                        a.state = AlertState::Idle;
                        let msg = format!("Alert Cleared: {0:?}, {1:?}={val:?}", a.msg, a.metric);
                        Self::alert(&mut self.alerter, &msg).await?
                    },
                (false, AlertState::Pending(_)) => { a.state = AlertState::Idle; },
                (false, AlertState::Idle) => (),
            }
        }
        Ok(()) 
    }
    async fn alert(alerter: &mut Matrix, msg: &str) -> Result<()> {
        println!("{msg:?}");
        alerter.send(msg).await?;
        Ok(())
    }
    async fn error(&mut self, msg: &str) -> () {
        let tn = SystemTime::now();
        // Skip if the message has been sent within the timeout
        match self.msgs.get(msg) {
            None => (),
            Some(t) => 
                if tn < *t + self.alert_timeout { return (); },
        };
        self.msgs.insert(msg.to_string(), tn);
        println!("{msg:?}");
        // We're already trying to log an error
        // so we just ignore the failure
        let _ = self.alerter.send(msg).await;
    }
    // This just helps keep the msgs list small and fast
    // If for some reason we have an Error with a unique component
    // this will expire it *eventually* so our memory doesn't grow
    // unbounded.
    fn gc(&mut self) {
        let tn = SystemTime::now();
        let mut todelete = None;
        for (msg, t) in self.msgs.iter() {
            if tn > *t + self.alert_timeout {
                todelete = Some(msg.clone());
                break;
            }
        }
        todelete.and_then(|x| self.msgs.remove(&x));
    }
    async fn disconnect(&mut self) -> Result<()> {
        self.alerter.disconnect().await?;
        Ok(())
    }
    async fn connect(&mut self) -> Result<()> {
        self.alerter.connect().await?;
        Ok(())
    }
    async fn check_rooms(&mut self) -> Result<()> {
        self.alerter.check_rooms().await?;
        Ok(())
    }
}

//***************************** Main ************************************

#[derive(Debug, Deserialize)]
struct Settings {
    influxdb: InfluxSettings, 
    solark: SolarkSettings,
    matrix: MatrixSettings,
    alerting: AlertSettings,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    use std::env;
    let args: Vec<String> = env::args().collect();
    let cfgpath =
        if args.len() < 2 {
            "/etc/solark_monitor.toml"
        } else {
            &args[1]
        };
    println!("Reading config file {cfgpath:?}");
    let cfg = config::Config::builder()
        .add_source(config::File::new(cfgpath, config::FileFormat::Toml))
        .build()?;
    let settings : Settings = cfg.try_deserialize()?;
    println!("config is {settings:?}");
    println!();
    let room_check_cycles = settings.matrix.room_check_cycles;
    let mut solark = Solark::new(&settings.solark);
    let mut (solark_data, solark_index) = solark::new_solarkdata();
    let mut influx = Influx::new(&settings.influxdb);
    let mut alerting = Alerting::new(&settings.alerting, Matrix::new(&settings.matrix))?;

    // NOW we start actually doing stuff
    // The initial connection is to *try* and set things up
    // We'll try and reconnect later if we fail now
    let solarkcf = solark.connect();
    let influxcf = influx.connect();
    let alertingcf = alerting.connect();
    let (solark_res, influx_res, alert_res) = tokio::join!(solarkcf, influxcf, alertingcf);
    for res in [solark_res, influx_res, alert_res] {
        match res {
            Ok(_) => (),
            Err(e) => alerting.error(&format!("Startup Error: {e:?}")).await,
        };
    }
    let mut interval = tokio::time::interval(Duration::from_secs(solark.cfg.poll_secs as u64));
    println!("Starting monitoring loop");
    let mut i = 1;
    // We need to start out with some values
    loop {
        //println!("");
        interval.tick().await;
        let values = match solark.read_all().await {
            Ok(v) => v,
            Err(e) => {
                alerting.error(&format!("Modbus Error: {e:?}")).await;
                solark.disconnect().await?;
                continue;
            }
        };
        //println!("values = {values:?}");
        // Read the next set of values while we process the last set
        let writef = influx.write_point(&values);
        let alertingf = alerting.check(&values);
        let (write_res, alert_res) = tokio::join!(writef, alertingf);
        match write_res {
            Ok(_) => (),
            Err(e) => {
                alerting.error(&format!("Influx Error: {e:?}")).await;
                influx.disconnect().await?;
            }
        }
        match alert_res {
            Ok(_) => (),
            Err(e) => {
                alerting.error(&format!("Alerting Error: {e:?}")).await;
                alerting.disconnect().await?;
            }
        }
        alerting.gc();
        // Kinda hacky, this should be a persistant connection and
        // an async callback, but the API for it is weird
        if room_check_cycles != 0 && i % room_check_cycles == 0 {
            alerting.check_rooms().await?;
        }
        i = i+1;
    }
}


