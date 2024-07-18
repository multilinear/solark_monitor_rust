use tokio;
use serde::Deserialize;
use std::time::{SystemTime, Duration};
use std::collections::BTreeMap;

type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

// *************************** Config ***************************************


//**************************** Solark (modbus) *******************************

#[derive(Debug)]
enum RegData {
    Bool(bool),
    Watts(i16),
    Percent(u16),
    Hz(u16),
}

type SolarkData = BTreeMap<String, RegData>;

#[derive(Debug, Deserialize, Clone)]
enum RegDataType {
    Bool,
    Bool64Bit, // Used to read the fault code and see if it's nonzero
    Watts,
    Percent,
    Hz,
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
    match self.ctx {
        Some(_) => return Ok(()),
        None => (),
    }
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
        RegDataType::Watts => RegData::Watts(
            // massage into a signed value
            if v[0] > (1<<(16-1))-1 {
                (v[0] as i32 - (1<<16)) as i16
            } else {
                v[0] as i16
            }),
        RegDataType::Percent => RegData::Percent(v[0]),
        RegDataType::Hz => RegData::Hz(v[0])
     })
	}

  pub async fn read_all(&mut self) -> Result<SolarkData> {
    self.connect().await?;
    // connect succeeded so ctx exists
    let ctx = &mut self.ctx.as_mut().unwrap();
    let mut results = SolarkData::new();
    // Sadly we can't parallelize this because self has to be borrowed as mutable
    // It's probably good though, since that would be a threadsafety problem
    // We can still do non-modbus stuff, like talk to influxdb, while awaiting
    for reg in &self.cfg.registers {
        let data = Solark::read_register(ctx, &reg).await?;
        results.insert(reg.metric.clone(), data); // TODO: get rid of clone here
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
        // Automatically reconnect if we're not connected
        self.connect().await?;
        // connect either created client, or errored out
        // so unwrap can't fail here
        let client = self.client.as_mut().unwrap();
        // Build up the list of points
        use influxdb2::models::DataPoint;
        let mut points = Vec::new();
        let timestamp = chrono::Utc::now().timestamp_nanos_opt().unwrap();
        for (metric, datum) in data.iter() {
            let point = DataPoint::builder(metric);
            points.push(
                self.cfg.tags.iter().fold(
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
                    }.timestamp(timestamp),
                    |point, tag| point.tag(&tag.key, &tag.val)
                ).build()?
            );
        }
        println!("writing points {points:?}");
        // TODO: We could avoid building the whole point list first
        client.write(&self.cfg.bucket, futures::stream::iter(points)).await?;
        Ok(())
    }
}

//***************************** Matrix **********************************
#[derive(Debug, Deserialize, Clone)]
struct MatrixSettings {
    // TODO: add some matrix settings
}

struct Matrix {
    cfg: MatrixSettings,
}
impl Matrix {
    fn new(cfg: &MatrixSettings) -> Self {
        Matrix{
            cfg: cfg.clone(),
        }
    }
    async fn connect(&mut self) -> Result<()> {
        // TODO write some matrix code
        Ok(())
    }
    async fn disconnect(&mut self) -> Result<()> {
        Ok(())
    }
    async fn send(&self, msg: &str) -> Result<()> {
        println!("TODO matrix not implemented msg: {msg:?}");
        Ok(())
    }
}

//***************************** Alerts **********************************
#[derive(Debug, Deserialize, Clone)]
struct AlertDesc {
    metric: String,
    check: String,
    msg: String, 
}

#[derive(Debug, Deserialize, Clone)]
struct AlertSettings {
    alerts: Vec<AlertDesc>,
    alert_timeout_secs: u32,
}

struct Alert {
    metric: String,
    fun: Box<dyn Fn(i64) -> bool>,
    fired: Option<SystemTime>,
    msg: String,
}

struct Alerting {
    alerter: Matrix,
    alerts: Vec<Alert>,
    msgs: BTreeMap<String, SystemTime>,
    alert_timeout: Duration,
}

impl Alerting {
    fn new(cfg: &AlertSettings, alerter: Matrix) -> Result<Self> {
        let mut alerts = Vec::with_capacity(cfg.alerts.len());
        for a in cfg.alerts.iter() {
            let val : i64 = a.check.chars().skip_while(|c| !c.is_digit(10))
                .collect::<String>().parse().expect(
                    "Failed to parse alert check expression from config");
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
                    fired: None,
                    msg: a.msg.clone(),
                }
            )
        }
        Ok(Self {
            alerter: alerter,
            alerts: alerts,
            alert_timeout: Duration::from_secs(cfg.alert_timeout_secs.into()),
            msgs: BTreeMap::new(),
        })
    }
    async fn check(&mut self, data: &SolarkData) -> Result<()> {
        let tn = SystemTime::now();
        // We're effectively implementing a very very shitty little
        // DSL here. Possibly we could plug in a rust crate that includes
        // an interpreter for something more real?
        for i in 1..self.alerts.len() {
            let a = &mut self.alerts[i];
            // This is a misconfiguration, so we want to crash
            // note that this will happen on the first run of "check"
            // so it won't surprise us when an alert fires or something
            let val = match data.get(&a.metric).expect(
                    &format!("Alert metric {0:?} not found in monitored data, fix your config", a.metric)
                  ) {
                RegData::Watts(v) => *v as i64,
                RegData::Percent(v) => *v as i64,
                RegData::Hz(v) => *v as i64,
                RegData::Bool(v) => *v as i64,
            };
            match ((a.fun)(val), a.fired) {
                (true, Some(t)) => 
                    if tn > t + self.alert_timeout {
                        Self::realert(&self.alerter, &a.msg, &a.metric, val).await?
                    },
                (true, None) => { 
                        a.fired = Some(tn);
                        Self::alert(&self.alerter, &a.msg, &a.metric, val).await?
                    },
                (false, Some(_)) => {
                        a.fired = None;
                        Self::clear_alert(&self.alerter, &a.msg, &a.metric, val).await?
                    },
                (false, None) => (),
            }
        }
        Ok(()) 
    }
    async fn alert(alerter: &Matrix, msg: &str, metric: &str, val: i64) -> Result<()> {
        alerter.send(&format!("Alert: {msg:?}, {metric:?}={val:?}")).await?;
        Ok(())
    }
    async fn realert(alerter: &Matrix, msg: &str, metric: &str, val: i64) -> Result<()> {
        alerter.send(&format!("Re-Alert: {msg:?}, {metric:?}={val:?}")).await?;
        Ok(())
    }
    async fn clear_alert(alerter: &Matrix, msg: &str, metric: &str, val: i64) -> Result<()> {
        alerter.send(&format!("Alert cleared: {msg:?}, {metric:?}={val:?}")).await?;
        Ok(())
    }
    async fn error(&mut self, msg: &str) -> () {
        // TODO: this is where we add backoff
        // maybe store a log of the messages and check against it
        // before resending
        // Make sure we print even if the alerter is disconnected
        println!("{msg:?}");
        let tn = SystemTime::now();
        // Skip if the message has been sent within the timeout
        match self.msgs.get(msg) {
            None => (),
            Some(t) => 
                if *t > tn + self.alert_timeout { return (); },
        };
        self.msgs.insert(msg.to_string(), tn);
        // We're already trying to log an error
        // so we just ignore the failure
        let _ = self.alerter.send(msg).await;
    }
    fn gc(&mut self) {
        let tn = SystemTime::now();
        let mut todelete = None;
        for (msg, t) in self.msgs.iter() {
            if *t > tn + self.alert_timeout {
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
    let cfgpath = "/etc/solark_monitor.toml";
    println!("Reading config file {cfgpath}");
    let cfg = config::Config::builder()
        .add_source(config::File::new(cfgpath, config::FileFormat::Toml))
        .build()?;
    let settings : Settings = cfg.try_deserialize()?;
    println!("config is {settings:?}");
    println!();
    let mut solark = Solark::new(&settings.solark);
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
    // We need to start out with some values
    loop {
        println!("");
        interval.tick().await;
        let values = match solark.read_all().await {
            Ok(v) => v,
            Err(e) => {
                alerting.error(&format!("Modbus Error: {e:?}")).await;
                solark.disconnect().await?;
                continue;
            }
        };
        println!("values = {values:?}");
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
    }
}


