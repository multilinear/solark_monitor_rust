use tokio;
use std::time::Duration;
use std::fmt;
use serde::Deserialize;

pub type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

// *************************** Config ***************************************


//**************************** Solark (modbus) *******************************

#[derive(Debug)]
enum RegData {
    Bool(bool),
    Watts(i16),
    Percent(u16),
    Hz(u16),
}

#[derive(Debug)]
struct SolarkDatum {
  name: String,
  value: RegData,
}

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
  name: String, // What to call the data
  addr: u16, // Address of the register
  datatype: RegDataType, // How to store the final data
}

#[derive(Debug, Deserialize, Clone)]
struct SolarkSettings {
  port: String,
  slave_id: u8,
  registers: Vec<SolarkReg>,
}

#[derive(Debug)]
struct Solark {
  serial: String,
  cfg: SolarkSettings,
	ctx: tokio_modbus::client::Context,
}	

/*
const SOLARK_REGISTERS : [SolarkReg; 9] = [
    SolarkReg{
        name: "Faults",
        addr: 103,
        datatype: RegDataType::Bool64Bit,
    },
    SolarkReg{
        name: "Gen Watts",
        addr: 166,
        datatype: RegDataType::Watts,
    },
    SolarkReg{
        name: "Grid Watts",
        addr: 169,
        datatype: RegDataType::Watts,
    },
    SolarkReg{
        name: "Inv Watts",
        addr: 175,
        datatype: RegDataType::Watts,
    },
    SolarkReg{
        name: "Load Watts",
        addr: 178,
        datatype: RegDataType::Watts,
    },
    SolarkReg{
        name: "Batt SOC",
        addr: 184,
        datatype: RegDataType::Percent,
    },
    SolarkReg{
        name: "Batt Watts",
        addr: 190,
        datatype: RegDataType::Watts,
    },
    SolarkReg{
        name: "Grid Live",
        addr: 194,
        datatype: RegDataType::Bool,
    },
    SolarkReg{
        name: "Gen Freq",
        addr: 196,
        datatype: RegDataType::Hz,
    },
];
*/

impl Solark {
	pub async fn connect(cfg: &SolarkSettings) -> Result<Solark> {
    use tokio_modbus::prelude::*;
    // connect
    let slave = Slave(cfg.slave_id); // Solark modbus ID

    let builder = tokio_serial::new(&cfg.port, 9600);
    let port = tokio_serial::SerialStream::open(&builder).unwrap();

    let mut ctx = rtu::attach_slave(port, slave);
    // check the connection by pulling the serial number
    let rsp = ctx.read_holding_registers(3, 5).await??;
    let mut serial = String::with_capacity(10);
    for nibble in rsp {
        // as u8 takes the bottom byte
        serial.push((nibble>>8) as u8 as char); 
        serial.push(nibble as u8 as char); 
    }
    return Ok(Solark{
        cfg: cfg.clone(),
        serial,
        ctx: ctx,
      });
	}

	pub async fn read_register(ctx: &mut tokio_modbus::client::Context, reg: &SolarkReg) -> Result<RegData> {
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

  pub async fn read_all(&mut self) -> Result<Vec<SolarkDatum>> {
    let mut results = Vec::new();
    // Sadly we can't parallelize this because self has to be borrowed as mutable
    // It's probably good though, since that would be a threadsafety problem
    // We can still do non-modbus stuff, like talk to influxdb, while awaiting
    for reg in &self.cfg.registers {
        let data = Solark::read_register(&mut self.ctx, &reg).await?;
        results.push(
            SolarkDatum{
                name: reg.name.clone(), // TODO: get rid of this string copy
                value: data,
            });
    }
    Ok(results)
  }

	pub async fn disconnect(&mut self) -> Result<()> {
	  println!("Disconnecting");
    self.ctx.disconnect().await??;
    Ok(())
	}
}

//***************************** Influx **********************************

#[derive(Debug, Deserialize)]
struct InfluxSettings {
    token: String,
    bucket: String,
    org: String,
    url: String,
}

struct Influx {
    bucket: String,
    client: influxdb2::Client,
}

impl Influx {
    async fn connect(cfg: &InfluxSettings) -> Result<Influx> {
        use influxdb2::Client;
        let client = Client::new(&cfg.url, &cfg.org, &cfg.token);
        Ok(Influx{
            bucket: cfg.bucket.clone(),
            client: client,
        })
    }
    async fn write_point(&mut self, data: &Vec<SolarkDatum>) -> Result<()> {
        use influxdb2::models::DataPoint;
        let mut points = Vec::new();
        let timestamp = chrono::Utc::now().timestamp_nanos_opt().unwrap();
        for datum in data {
            let point = DataPoint::builder(&datum.name);
            points.push(match datum.value {
                RegData::Bool(v) => 
                    point.tag("units", "Bool")
                    .field("value", if v {1} else {0}),
                RegData::Watts(v) =>
                    point.tag("units", "Watts")
                    .field("value", v as i64),
                RegData::Percent(v) =>
                    point.tag("units", "Percent")
                    .field("value", v as i64),
                RegData::Hz(v) =>
                    point.tag("units", "Hz")
                    .field("value", v as i64),
            }.tag("Solark", "1") .timestamp(timestamp).build()?);
        }
        println!("writing points {points:?}");
        // TODO: We could avoid building the whole point list first
        self.client.write(&self.bucket, futures::stream::iter(points)).await?;
        Ok(())
    }
}

//***************************** Main ************************************

async fn run_loop(solark: &mut Solark, influx: &mut Influx) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        println!("");
        interval.tick().await;
        let values = solark.read_all().await?;
        println!("values = {values:?}");
        // TODO: this should be done concurrently with the next read
        // Maybe switch to using channels? We could fire off a seperate loop
        // for each service, the two others waiting for data from this one
        // Failure handling would be a lot more elegant that way
        influx.write_point(&values).await?;
    }
}

#[derive(Debug)]
struct MyError;
impl fmt::Display for MyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Parse error")
    }
}
impl std::error::Error for MyError {
    fn description(&self) -> &str {
        return "This is a parse error";
    }
}

#[derive(Debug, Deserialize)]
struct Settings {
    influxdb: InfluxSettings, 
    solark: SolarkSettings,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cfgpath = "/etc/solark_monitor.toml";
    println!("Reading config file {cfgpath}");
    let cfg = config::Config::builder()
        .add_source(config::File::new(cfgpath, config::FileFormat::Toml))
        .build()?;
    println!("deserializing");
    let settings : Settings = cfg.try_deserialize()?;
    println!("config is {settings:?}");

		let mut solark = Solark::connect(&settings.solark).await?;
    let mut influx = Influx::connect(&settings.influxdb).await?;
    println!("connected to {solark:?}");
    // We put this in a seperate function to simplify error propogation
    // This way it's easy to disconnect on error
    // TODO: we'll want to recover from these errors eventually, e.g. if
    // you disconnect the cable it should keep trying.
    let res = run_loop(&mut solark, &mut influx).await;
    let dres = solark.disconnect().await;
    match res {
        Ok(_) => return dres,
        Err(_) => return res,
    }
}


