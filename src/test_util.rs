pub struct TempEnvVar {
    pub key: String,
    pub initial_value: Option<String>,
}

impl TempEnvVar {
    pub fn new(key: &str) -> Self {
        println!("INIT KEY: {}", key);
        let initial_value = std::env::var(key).ok();
        std::env::remove_var(key);
        Self {
            key: key.into(),
            initial_value,
        }
    }

    pub fn with(self, value: &str) -> Self {
        println!("SET KEY: {}", self.key);
        std::env::set_var(self.key.as_str(), value);
        self
    }
}

impl Drop for TempEnvVar {
    fn drop(&mut self) {
        println!("DROP KEY: {}", self.key);
        match self.initial_value.as_ref() {
            Some(value) => std::env::set_var(self.key.as_str(), value),
            None => std::env::remove_var(self.key.as_str()),
        }
    }
}
