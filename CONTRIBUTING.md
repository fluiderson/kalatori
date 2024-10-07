## Version Bumping and Release Process

When you make changes that require a new version of the project, follow these steps to bump the version:

1. **Update the Version Number**:
    - Update version in `Cargo.toml`

2. **Update the Changelog**:
    - Run `git cliff <range> --tag <new-version>` to generate the changelog for the new version.
   ```bash
   # For example in my case the origin of main repository marked as main,
   # som main/main is the main branch of the main repository.
   # 2.1.2 is version example.  
    git cliff main/main..HEAD --tag 2.1.2 -p CHANGELOG.md 
   ```
    - Review the changelog to ensure that the description is meaningful

3. **Add version related changes to commit**:
   ```bash
   git add CHANGELOG.md Cargo.toml Cargo.lock
   git commit -m "chore: bump version to 2.1.2"
   git push origin <branch-name>
    ```

4. **Tag the version**:
    ```bash
    git tag -a v2.1.2 -m "Release version 2.1.2"
    git push origin v2.1.2
    ```

