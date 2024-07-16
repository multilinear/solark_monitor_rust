use tokio;
use std::time::Duration;

pub type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

//**************************** Solark (modbus) *******************************

#[derive(Debug)]
enum RegDataType {
    Bool,
    Bool64Bit, // Used to read the fault code and see if it's nonzero
    Watts,
    Percent,
    Hz,
}

#[derive(Debug)]
enum RegData {
    Bool(bool),
    Watts(i16),
    Percent(u16),
    Hz(u16),
}

#[derive(Debug)]
struct SolarkReg<'a> {
  name: &'a str, // What to call the data
  addr: u16, // Address of the register
  datatype: RegDataType, // How to store the final data
}

#[derive(Debug)]
struct SolarkDatum<'a> {
  name: &'a str,
  value: RegData,
}

#[derive(Debug)]
struct Solark<'a> {
  serial: String,
	port: &'a str, 
	slave_id: i8,
	ctx: tokio_modbus::client::Context,
}	

const SOLARK_REGISTERS : [SolarkReg<'static>; 9] = [
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

impl Solark<'_> {
	pub async fn connect() -> Result<Solark<'static>> {
    use tokio_modbus::prelude::*;
    // connect
		let port_str = "/dev/ttyUSB0";
		let slave_id = 0x1;
    let slave = Slave(slave_id); // Solark modbus ID

    let builder = tokio_serial::new(port_str, 9600);
    let port = tokio_serial::SerialStream::open(&builder).unwrap();

    let mut ctx = rtu::attach_slave(port, slave);
    // check the connection by pulling the serial number
    let rsp = ctx.read_holding_registers(3, 5).await??;
    let mut serial = String::with_capacity(10);
    for nibble in rsp {
       serial.push(((nibble&(255<<8))>>8) as u8 as char); 
       serial.push((nibble&255) as u8 as char); 
    }
    return Ok(Solark{
        serial,
        port: port_str,
        slave_id: 0x1,
        ctx: ctx,
      });
	}

	pub async fn read_register(&mut self, reg: &SolarkReg<'_>) -> Result<RegData> {
    use tokio_modbus::prelude::*;
    let count = match reg.datatype {
            RegDataType::Bool64Bit => 4,
            _ => 1,
        };
    println!("Reading the holding registers {reg:?}");
    let v = self.ctx.read_holding_registers(reg.addr, count).await??;
    Ok(match reg.datatype {
        RegDataType::Bool64Bit => RegData::Bool(v[0] | v[1] | v[2] | v[3] != 0),
        RegDataType::Bool => RegData::Bool(v[0] != 0),
        RegDataType::Watts => RegData::Watts(
            // massage into a signed value
            if v[0] > (1<<(16-1))-1 {
                // If this were one byte, then we'd subtract 128
                // then convert to signed
                // then subtract 128 again
                // This avoids overflow, and we can see 255 becomes
                // -1, and 254 becomes -2 as it should.
                (v[0] - (1<<(16-1))) as i16 - (1<<(16-1))
            } else {
                v[0] as i16
            }),
        RegDataType::Percent => RegData::Percent(v[0]),
        RegDataType::Hz => RegData::Hz(v[0])
     })
	}

  pub async fn read_all(&mut self) -> Result<Vec<SolarkDatum>> {
    println!("Reading all");
    let mut results = Vec::new();
    // Sadly we can't parallelize this because self has to be borrowed as mutable
    // It's probably good though, since that would be a threadsafety problem
    // We can still do non-modbus stuff, like talk to influxdb, while awaiting
    for reg in SOLARK_REGISTERS {
        let data = self.read_register(&reg).await?;
        results.push(
            SolarkDatum{
                name: reg.name,
                value: data,
            });
    }
    Ok(results)
  }

	pub async fn disconnect(&mut self) -> Result<()> {
	  println!("Disconnecting");
    self.ctx.disconnect().await?;
		Ok(())
	}
}

//***************************** Influx **********************************

struct Influx<'a> {
    url: &'a str,
    bucket: &'a str,
    client: influxdb2::Client,
}

impl Influx<'_> {
    async fn connect() -> Result<Influx<'static>> {
        use influxdb2::Client;
        let url = "http://localhost:8083";
        let org = "smalladventures";
        let token = "";
        let client = Client::new(url, org, token);
        Ok(Influx{
            url: url,
            bucket: "power",
            client: client,
        })
    }
    async fn write_point(&mut self, data: &Vec<SolarkDatum<'_>>) -> Result<()> {
        use influxdb2::models::DataPoint;
        let mut points = Vec::new();
        for datum in data {
            let point = DataPoint::builder(datum.name);
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
            }.build()?);
        }
        // TODO: We could avoid building the whole point list first
        self.client.write(self.bucket, futures::stream::iter(points)).await?;
        Ok(())
    }
}

//***************************** Main ************************************

async fn run_loop(solark: &mut Solark<'_>, influx: &mut Influx<'_>) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
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


#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
		let mut solark = Solark::connect().await?;
    let mut influx = Influx::connect().await?;
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


