use snapbox::cmd::Command;

#[test]
fn smoke() {
    let bmat = snapbox::cmd::cargo_bin("buildomat");

    Command::new(bmat).arg("--help").assert().success();
}

mod cache {
    mod restore {
        use snapbox::cmd::Command;

        #[test]
        fn smoke() -> Result<(), Box<dyn std::error::Error>> {
            let bmat = snapbox::cmd::cargo_bin("buildomat");

            let temp_dir = tempfile::tempdir()?;

            Command::new(bmat)
                .arg("admin")
                .arg("cache")
                .arg("restore")
                .arg("--help")
                .env("INPUT_URL", "lol")
                .env("INPUT_SECRET", "lol")
                .env("INPUT_ADMIN_TOKEN", "lol")
                .current_dir(&temp_dir)
                .assert()
                .success();

            Ok(())
        }
    }

    mod upload {
        use snapbox::cmd::Command;

        type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

        #[test]
        fn smoke() -> TestResult {
            let bmat = snapbox::cmd::cargo_bin("buildomat");

            let temp_dir = tempfile::tempdir()?;

            Command::new(bmat)
                .arg("admin")
                .arg("cache")
                .arg("upload")
                .arg("--help")
                .env("INPUT_URL", "lol")
                .env("INPUT_SECRET", "lol")
                .env("INPUT_ADMIN_TOKEN", "lol")
                .current_dir(&temp_dir)
                .assert()
                .success();

            Ok(())
        }
    }
}
