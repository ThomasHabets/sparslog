use anyhow::Result;
use tokio::io::AsyncReadExt;

type Complex = num::complex::Complex<f32>;

struct StreamWriter<T> {
    data: Vec<T>,
}

impl<T: Copy> StreamWriter<T> {
    fn new() -> Self {
        Self{
            data: Vec::new(),
        }
    }
    async fn write(&mut self, data: &[T]) -> Result<()> {
        self.data.extend(data);
        Ok(())
    }
}

struct TCPSource<T> {
    stream: tokio::net::TcpStream,
    t: T,
}

impl<T: Copy> TCPSource<T> {
    async fn new(t2: T) -> Result<Self> {
        Ok(Self{
            t: t2,
            stream: tokio::net::TcpStream::connect(
	        ("localhost", 2000)
            ).await?,
        })
    }
    async fn work(&mut self, w: &mut StreamWriter<T>) -> Result<()> {
        let mut buf = [0u8; 8192];
        let n = self.stream.read(&mut buf).await?;
        let data = &buf[0..n];
        let mut v = Vec::new();
        for c in 0..(n/8) {
            let a = 8*c;
            let b = a + 4;
            let i = f32::from_be_bytes(data[a..b].try_into()?);
            let q = f32::from_be_bytes(data[a+4..b+4].try_into()?);
            v.push(Complex::new(i,q));
        }
        println!("Read a bunch {}", data.len());
        w.write(v.as_slice()).await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("Hello, world!");

    let mut src = TCPSource::new(Complex::new(0.0,0.0)).await?;
    let mut s = StreamWriter::new();
    loop {
        src.work(&mut s).await?;
    }
    //Ok(())
}
