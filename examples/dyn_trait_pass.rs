use cached::proc_macro::cached;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum ProcessorError {
    #[error("error wile processing task :{0}")]
    TaskError(String),
}

trait AnyTaskProcessor {
    type Error;

    fn execute(&self) -> Result<String, ProcessorError>;
}

struct CustomProcessor;

impl AnyTaskProcessor for CustomProcessor {
    type Error = ProcessorError;

    fn execute(&self) -> Result<String, Self::Error> {
        Ok(String::from("hello world"))
    }
}

#[cached(
    time = 100,             // Expires after 100 seconds
    size = 1,               // Cache size (1) elements
    result = true,          // Cache the Result type
    key = "i32",            // Necessary option for caching method result
    convert = r##"{ 1 }"##  // Necessary option for key -> used static integer for example only
)]
fn cached_execute(
    processor: &(dyn AnyTaskProcessor<Error = ProcessorError> + Send + Sync),
) -> Result<String, ProcessorError> {
    std::thread::sleep(std::time::Duration::from_secs(2));
    let result = processor.execute()?;
    Ok(result)
}

fn main() -> Result<(), Box<dyn std::error::Error>>{
    let mean_delay = 100u128;

    let custom_processor = CustomProcessor {};

    let start_time = std::time::Instant::now();
    let result = cached_execute(&custom_processor)?;
    let elapsed = start_time.elapsed();
    assert_eq!(&result, "hello world");
    assert!(elapsed.as_millis() >= mean_delay);

    let start_time = std::time::Instant::now();
    let result = cached_execute(&custom_processor)?;
    let elapsed = start_time.elapsed();
    assert_eq!(&result, "hello world");
    assert!(elapsed.as_millis() < mean_delay);

    println!("done!");

    Ok(())
}
