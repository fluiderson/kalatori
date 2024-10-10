# Dockerfile
FROM node:16-alpine

# Set the working directory
WORKDIR /app

# Install Chopsticks
RUN npm install -g @acala-network/chopsticks

# Expose ports for both instances
EXPOSE 8000 8500 9000 9500

# Default command (this will be overridden by docker-compose)
CMD ["chopsticks", "-c", "/app/config.yml", "-p", "9000"]
