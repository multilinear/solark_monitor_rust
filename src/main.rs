//use tokio_serial::SerialStream;
use tokio_modbus::prelude::*;
use tokio;
//use futures::future;

pub type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
enum RegDataType {
    Bool,
    IWatts,
    UWatts,
    Percent,
    Hz,
}

#[derive(Debug)]
enum RegData {
    Bool(bool),
    IWatts(i64),
    UWatts(u64),
    Percent(u64),
    Hz(u64),
}

#[derive(Debug)]
struct SolarkReg<'a> {
  name: &'a str,
  addr: u16,
  count: u16,
  datatype: RegDataType,
}

#[derive(Debug)]
struct SolarkData<'a> {
  name: &'a str,
  data: RegData,
}

#[derive(Debug)]
struct Solark<'a> {
	port: &'a str, 
	slave_id: i8,
	ctx: tokio_modbus::client::Context,
}	

const SOLARK_REGISTERS : [SolarkReg<'static>; 1] = [
    SolarkReg{
        name: "Faults",
        addr: 103,
        count: 4,
        datatype: RegDataType::Bool,
    }
];

impl Solark<'_> {
	pub async fn connect() -> Result<Solark<'static>> {
		let port_str = "/dev/ttyUSB0";
		let slave_id = 0x1;
    let slave = Slave(slave_id); // Solark modbus ID

    let builder = tokio_serial::new(port_str, 19200);
    let port = tokio_serial::SerialStream::open(&builder).unwrap();

		let mut solark = Solark{
        port: port_str,
        slave_id: 0x1,
        ctx: rtu::attach_slave(port, slave),
      };
    println!("connected, context={solark:?}");
    println!("Reading the holding registers for serial#");
    let rsp = solark.ctx.read_holding_registers(3, 5).await??;
    println!("got {rsp:?}");
    return Ok(solark);
	}

	pub async fn read_register(&mut self, reg: &SolarkReg<'_>) -> Result<RegData> {
    println!("Reading the holding registers {reg:?}");
    let rsp = self.ctx.read_holding_registers(reg.addr, reg.count).await??;
    println!("Read the holding registers");
    let mut answer : u64 = 0;
    let mut bit : u8 = 0;
    for v in rsp {
       answer += (v as u64) << bit;
       bit += 16;
    }
    // TODO move Ok back out once I get this figured out
    match reg.datatype {
        RegDataType::Bool => Ok(RegData::Bool(answer != 0)),
        RegDataType::IWatts => Ok(RegData::IWatts(
            // massage into a signed value
            if answer > 1<<(bit-1)-1 {
                // If this were one byte, then we'd subtract 128
                // then convert to signed
                // then subtract 128 again
                // This avoids overflow, and we can see 255 becomes
                // -1, and 254 becomes -2 as it should.
                (answer - (1<<(bit-1))) as i64 - (1<<(bit-1))
            } else {
                answer as i64
            })),
        RegDataType::UWatts => Ok(RegData::UWatts(answer)),
        RegDataType::Percent =>Ok(RegData::Percent(answer)),
        RegDataType::Hz => Ok(RegData::Hz(answer))
    }
	}

  pub async fn read_all(&mut self) -> Result<Vec<SolarkData>> {
    println!("Reading all");
    let mut results = Vec::new();
    // Sadly we can't parallelize this because self has to be borrowed as mutable
    // It's probably good though, since that would be a threadsafety problem
    // We can still do non-modbus stuff, like talk to influxdb, while awaiting
    for reg in SOLARK_REGISTERS {
        let data = self.read_register(&reg).await?;
        results.push(
            SolarkData{
                name: reg.name,
                data: data,
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
		let mut solark = Solark::connect().await?;
    println!("connected to {solark:?}");

    println!("Reading a sensor value");
    let value = solark.read_all().await?;
    println!("Sensor value is: {value:?}");

    println!("Disconnecting");
    let foo = solark.disconnect().await?;
    println!("disconnect is {foo:?}");
		Ok(())
}


