use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

//**************************** Solark (modbus) *******************************

#[derive(Debug)]
pub enum RegData {
    Bool(bool),
    Watts(i16),
    Percent(u16),
    Hz(u16),
    Volts(i16), // Stored at 10'ths of a volt, annoyingly
}

// We store the actual data in a vector
// This way we can cache the index and avoid doing
// tree lookups every cycle
type SolarkData = Vec<RegData>;

#[derive(Debug, Deserialize, Clone)]
pub enum RegDataType {
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
pub struct SolarkSettings {
  poll_secs: u32,
  port: String,
  slave_id: u8,
  registers: Vec<SolarkReg>,
}

#[derive(Debug)]
pub struct Solark {
  cfg: SolarkSettings,
  serial: Option<String>,
	ctx: Option<tokio_modbus::client::Context>,
}	

impl Solark {
  pub fn new(cfg: &SolarkSettings) -> Self {
    Self {
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

  pub async fn read_all(&mut self) -> Result<SolarkData> {
    self.connect().await?;
    // connect succeeded so ctx exists
    let ctx = &mut self.ctx.as_mut().unwrap();
    let mut results = Vec::with_capacity(self.cfg.registers.len());
    for reg in &self.cfg.registers {
        results.push(Solark::read_register(ctx, &reg).await?);
    }
    Ok(results)
  }

	pub async fn disconnect(&mut self) -> Result<()> {
    match &mut self.ctx {
      Some(ctx) => {
	        println!("Disconnecting");
          ctx.disconnect().await??;
        },
      None => (),
    }
    Ok(())
	}

  // Spits out an ordered list of the metric names
  // This list has the same order as the result of read_all()
  pub fn make_names_list(&self) -> Vec<String> {
    let mut names = Vec::with_capacity(self.cfg.registers.len());
    for reg in &self.cfg.registers {
        names.push(reg.metric.clone());
    }
    return names;
  }

  // Returns a map from metric name to metric index
  pub fn make_metric_lookup(&self) -> BTreeMap<String, usize> {
    let mut lookup = BTreeMap::new();
    let mut i = 0;
    for reg in &self.cfg.registers {
        lookup.insert(reg.metric.clone(), i);
        i = i + 1;
    }
    lookup
  }

  pub fn get_poll_duration(&self) -> Duration {
      Duration::from_secs(self.cfg.poll_secs as u64)
  }
}
