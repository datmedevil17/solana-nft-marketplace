#![allow(unexpected_cfgs)]
#![allow(deprecated)]
use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer, Mint};
use anchor_spl::associated_token::AssociatedToken;

declare_id!("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");

#[program]
pub mod escrow {
    use super::*;

    /// Create a new escrow account
    pub fn create_escrow(
        ctx: Context<CreateEscrow>,
        escrow_type: EscrowType,
        duration: Option<i64>,
    ) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        let clock = Clock::get()?;

        escrow.authority = ctx.accounts.authority.key();
        escrow.escrow_type = escrow_type;
        escrow.created_at = clock.unix_timestamp;
        escrow.expires_at = duration.map(|d| clock.unix_timestamp + d);
        escrow.nft_mint = None;
        escrow.sol_amount = 0;
        escrow.is_released = false;
        escrow.is_emergency_withdrawn = false;
        escrow.bump = ctx.bumps.escrow;

        emit!(EscrowCreated {
            escrow: escrow.key(),
            authority: escrow.authority,
            escrow_type,
            created_at: escrow.created_at,
            expires_at: escrow.expires_at,
        });

        Ok(())
    }

    /// Deposit NFT into escrow
    pub fn deposit_nft(ctx: Context<DepositNft>) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        
        // Validate escrow state
        require!(!escrow.is_released, EscrowError::EscrowAlreadyReleased);
        require!(!escrow.is_emergency_withdrawn, EscrowError::EscrowEmergencyWithdrawn);
        require!(escrow.nft_mint.is_none(), EscrowError::NftAlreadyDeposited);
        
        // Check expiry
        if let Some(expires_at) = escrow.expires_at {
            let clock = Clock::get()?;
            require!(clock.unix_timestamp < expires_at, EscrowError::EscrowExpired);
        }

        // Transfer NFT to escrow
        let transfer_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.depositor_token_account.to_account_info(),
                to: ctx.accounts.escrow_token_account.to_account_info(),
                authority: ctx.accounts.depositor.to_account_info(),
            },
        );
        token::transfer(transfer_ctx, 1)?;

        // Update escrow state
        escrow.nft_mint = Some(ctx.accounts.mint.key());

        emit!(NftDeposited {
            escrow: escrow.key(),
            mint: ctx.accounts.mint.key(),
            depositor: ctx.accounts.depositor.key(),
        });

        Ok(())
    }

    /// Deposit SOL into escrow
    pub fn deposit_sol(ctx: Context<DepositSol>, amount: u64) -> Result<()> {
        // Validate escrow state and get current amount
        let (current_sol, escrow_key, depositor_key) = {
            let escrow = &ctx.accounts.escrow;
            
            require!(!escrow.is_released, EscrowError::EscrowAlreadyReleased);
            require!(!escrow.is_emergency_withdrawn, EscrowError::EscrowEmergencyWithdrawn);
            require!(amount > 0, EscrowError::InvalidAmount);
            
            // Check expiry
            if let Some(expires_at) = escrow.expires_at {
                let clock = Clock::get()?;
                require!(clock.unix_timestamp < expires_at, EscrowError::EscrowExpired);
            }

            (escrow.sol_amount, escrow.key(), ctx.accounts.depositor.key())
        };

        // Transfer SOL to escrow
        let transfer_ctx = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            anchor_lang::system_program::Transfer {
                from: ctx.accounts.depositor.to_account_info(),
                to: ctx.accounts.escrow.to_account_info(),
            },
        );
        anchor_lang::system_program::transfer(transfer_ctx, amount)?;

        // Update escrow state after transfer
        let escrow = &mut ctx.accounts.escrow;
        escrow.sol_amount = current_sol.checked_add(amount)
            .ok_or(EscrowError::MathOverflow)?;

        emit!(SolDeposited {
            escrow: escrow_key,
            depositor: depositor_key,
            amount,
            total_sol: escrow.sol_amount,
        });

        Ok(())
    }

    /// Release assets from escrow (requires authority or multi-sig)
    pub fn release_assets(ctx: Context<ReleaseAssets>) -> Result<()> {
        // Validate escrow state and extract needed values
        let (nft_mint, sol_amount, authority, created_at, bump, escrow_key, authority_key, recipient_owner) = {
            let escrow = &ctx.accounts.escrow;
            
            require!(!escrow.is_released, EscrowError::EscrowAlreadyReleased);
            require!(!escrow.is_emergency_withdrawn, EscrowError::EscrowEmergencyWithdrawn);
            
            // Authority check
            require!(
                ctx.accounts.authority.key() == escrow.authority,
                EscrowError::Unauthorized
            );

            (
                escrow.nft_mint,
                escrow.sol_amount,
                escrow.authority,
                escrow.created_at,
                escrow.bump,
                escrow.key(),
                ctx.accounts.authority.key(),
                ctx.accounts.recipient_token_account.owner,
            )
        };

        let escrow_seeds = &[
            b"escrow",
            authority.as_ref(),
            &created_at.to_le_bytes(),
            &[bump],
        ];
        let signer = &[&escrow_seeds[..]];

        // Release NFT if present
        if nft_mint.is_some() {
            let nft_transfer_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.escrow_token_account.to_account_info(),
                    to: ctx.accounts.recipient_token_account.to_account_info(),
                    authority: ctx.accounts.escrow.to_account_info(),
                },
                signer,
            );
            token::transfer(nft_transfer_ctx, 1)?;
        }

        // Release SOL if present
        if sol_amount > 0 {
            let sol_transfer_ctx = CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.escrow.to_account_info(),
                    to: ctx.accounts.sol_recipient.to_account_info(),
                },
                signer,
            );
            anchor_lang::system_program::transfer(sol_transfer_ctx, sol_amount)?;
        }

        // Update escrow state after transfers
        let escrow = &mut ctx.accounts.escrow;
        escrow.is_released = true;

        emit!(AssetsReleased {
            escrow: escrow_key,
            authority: authority_key,
            nft_mint,
            sol_amount,
            nft_recipient: recipient_owner,
            sol_recipient: ctx.accounts.sol_recipient.key(),
        });

        Ok(())
    }

    /// Emergency withdraw (admin only, for stuck assets)
    pub fn emergency_withdraw(ctx: Context<EmergencyWithdraw>) -> Result<()> {
        // Validate escrow state and extract needed values
        let (nft_mint, sol_amount, authority, created_at, bump, escrow_key, admin_key) = {
            let escrow = &ctx.accounts.escrow;
            
            require!(!escrow.is_released, EscrowError::EscrowAlreadyReleased);
            require!(!escrow.is_emergency_withdrawn, EscrowError::EscrowAlreadyEmergencyWithdrawn);
            
            // Only marketplace admin can emergency withdraw
            require!(
                ctx.accounts.marketplace.authority == ctx.accounts.admin.key(),
                EscrowError::Unauthorized
            );

            (
                escrow.nft_mint,
                escrow.sol_amount,
                escrow.authority,
                escrow.created_at,
                escrow.bump,
                escrow.key(),
                ctx.accounts.admin.key(),
            )
        };

        let escrow_seeds = &[
            b"escrow",
            authority.as_ref(),
            &created_at.to_le_bytes(),
            &[bump],
        ];
        let signer = &[&escrow_seeds[..]];

        // Emergency withdraw NFT if present
        if nft_mint.is_some() {
            let nft_transfer_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.escrow_token_account.to_account_info(),
                    to: ctx.accounts.recovery_token_account.to_account_info(),
                    authority: ctx.accounts.escrow.to_account_info(),
                },
                signer,
            );
            token::transfer(nft_transfer_ctx, 1)?;
        }

        // Emergency withdraw SOL if present
        if sol_amount > 0 {
            let sol_transfer_ctx = CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.escrow.to_account_info(),
                    to: ctx.accounts.recovery_sol_account.to_account_info(),
                },
                signer,
            );
            anchor_lang::system_program::transfer(sol_transfer_ctx, sol_amount)?;
        }

        // Update escrow state after transfers
        let escrow = &mut ctx.accounts.escrow;
        escrow.is_emergency_withdrawn = true;

        emit!(EmergencyWithdrawal {
            escrow: escrow_key,
            admin: admin_key,
            nft_mint,
            sol_amount,
            recovery_account: ctx.accounts.recovery_sol_account.key(),
        });

        Ok(())
    }

    /// Get escrow status
    pub fn get_escrow_status(ctx: Context<GetEscrowStatus>) -> Result<EscrowStatus> {
        let escrow = &ctx.accounts.escrow;
        let clock = Clock::get()?;
        
        let status = if escrow.is_released {
            EscrowStatus::Released
        } else if escrow.is_emergency_withdrawn {
            EscrowStatus::EmergencyWithdrawn
        } else if let Some(expires_at) = escrow.expires_at {
            if clock.unix_timestamp >= expires_at {
                EscrowStatus::Expired
            } else {
                EscrowStatus::Active
            }
        } else {
            EscrowStatus::Active
        };

        Ok(status)
    }
}

#[derive(Accounts)]
pub struct CreateEscrow<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + EscrowState::INIT_SPACE,
        seeds = [b"escrow", authority.key().as_ref(), &Clock::get()?.unix_timestamp.to_le_bytes()],
        bump
    )]
    pub escrow: Account<'info, EscrowState>,
    
    #[account(mut)]
    pub authority: Signer<'info>,
    
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

// Fixed: Added #[derive(Accounts)] macro
#[derive(Accounts)]
pub struct DepositNft<'info> {
    #[account(
        mut,
        seeds = [b"escrow", escrow.authority.as_ref(), &escrow.created_at.to_le_bytes()],
        bump = escrow.bump
    )]
    pub escrow: Account<'info, EscrowState>,
    
    // Removed has_one constraint since depositor might be different from authority
    pub authority: Signer<'info>,
    
    #[account(mut)]
    pub depositor: Signer<'info>,
    
    pub mint: Account<'info, Mint>,
    
    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = depositor,
        constraint = depositor_token_account.amount == 1 @ EscrowError::InvalidAmount
    )]
    pub depositor_token_account: Account<'info, TokenAccount>,
    
    #[account(
        init_if_needed,
        payer = depositor,
        associated_token::mint = mint,
        associated_token::authority = escrow
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,
    
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DepositSol<'info> {
    #[account(
        mut,
        seeds = [b"escrow", escrow.authority.as_ref(), &escrow.created_at.to_le_bytes()],
        bump = escrow.bump
    )]
    pub escrow: Account<'info, EscrowState>,
    
    pub authority: Signer<'info>,
    
    #[account(mut)]
    pub depositor: Signer<'info>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ReleaseAssets<'info> {
    #[account(
        mut,
        seeds = [b"escrow", escrow.authority.as_ref(), &escrow.created_at.to_le_bytes()],
        bump = escrow.bump
    )]
    pub escrow: Account<'info, EscrowState>,
    
    pub authority: Signer<'info>,
    
    #[account(mut)]
    pub escrow_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub recipient_token_account: Account<'info, TokenAccount>,
    
    /// CHECK: SOL recipient account
    #[account(mut)]
    pub sol_recipient: AccountInfo<'info>,
    
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct EmergencyWithdraw<'info> {
    #[account(
        mut,
        seeds = [b"escrow", escrow.authority.as_ref(), &escrow.created_at.to_le_bytes()],
        bump = escrow.bump
    )]
    pub escrow: Account<'info, EscrowState>,
    
    pub admin: Signer<'info>,
    
    pub marketplace: Account<'info, marketplace::MarketplaceState>,
    
    #[account(mut)]
    pub escrow_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub recovery_token_account: Account<'info, TokenAccount>,
    
    /// CHECK: Recovery SOL account
    #[account(mut)]
    pub recovery_sol_account: AccountInfo<'info>,
    
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct GetEscrowStatus<'info> {
    #[account(
        seeds = [b"escrow", escrow.authority.as_ref(), &escrow.created_at.to_le_bytes()],
        bump = escrow.bump
    )]
    pub escrow: Account<'info, EscrowState>,
}

#[account]
#[derive(InitSpace)]
pub struct EscrowState {
    pub authority: Pubkey,              // 32
    pub escrow_type: EscrowType,        // 1 + size
    pub created_at: i64,                // 8
    pub expires_at: Option<i64>,        // 1 + 8
    pub nft_mint: Option<Pubkey>,       // 1 + 32
    pub sol_amount: u64,                // 8
    pub is_released: bool,              // 1
    pub is_emergency_withdrawn: bool,   // 1
    pub bump: u8,                       // 1
}

impl EscrowState {
    pub const INIT_SPACE: usize = 32 + 1 + 1 + 8 + 1 + 8 + 1 + 32 + 8 + 1 + 1 + 1; // 95 bytes
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum EscrowType {
    Listing,
    Auction,
    DirectSale,
    Swap,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscrowStatus {
    Active,
    Expired,
    Released,
    EmergencyWithdrawn,
}

#[event]
pub struct EscrowCreated {
    pub escrow: Pubkey,
    pub authority: Pubkey,
    pub escrow_type: EscrowType,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

#[event]
pub struct NftDeposited {
    pub escrow: Pubkey,
    pub mint: Pubkey,
    pub depositor: Pubkey,
}

#[event]
pub struct SolDeposited {
    pub escrow: Pubkey,
    pub depositor: Pubkey,
    pub amount: u64,
    pub total_sol: u64,
}

#[event]
pub struct AssetsReleased {
    pub escrow: Pubkey,
    pub authority: Pubkey,
    pub nft_mint: Option<Pubkey>,
    pub sol_amount: u64,
    pub nft_recipient: Pubkey,
    pub sol_recipient: Pubkey,
}

#[event]
pub struct EmergencyWithdrawal {
    pub escrow: Pubkey,
    pub admin: Pubkey,
    pub nft_mint: Option<Pubkey>,
    pub sol_amount: u64,
    pub recovery_account: Pubkey,
}

#[error_code]
pub enum EscrowError {
    #[msg("Escrow already released")]
    EscrowAlreadyReleased,
    #[msg("Escrow emergency withdrawn")]
    EscrowEmergencyWithdrawn,
    #[msg("Escrow already emergency withdrawn")]
    EscrowAlreadyEmergencyWithdrawn,
    #[msg("NFT already deposited")]
    NftAlreadyDeposited,
    #[msg("Escrow expired")]
    EscrowExpired,
    #[msg("Invalid amount")]
    InvalidAmount,
    #[msg("Math overflow occurred")]
    MathOverflow,
    #[msg("Unauthorized access")]
    Unauthorized,
}

// External module reference for CPI
pub mod marketplace {
    use super::*;
    
    #[account]
    #[derive(InitSpace)]
    pub struct MarketplaceState {
        pub authority: Pubkey,
        pub treasury: Pubkey,
        pub platform_fee: u16,
        pub total_volume: u64,
        pub total_trades: u64,
        pub is_paused: bool,
        pub bump: u8,
    }
}